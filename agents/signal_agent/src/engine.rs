// agents/signal_agent/src/engine.rs
// Core Signal Agent: subscribes to PosteriorUpdate + MarketUpdate events,
// computes Expected Value and Kelly position size, and emits TradeSignal events.

use chrono::Utc;
use common::{Event, EventBus, TradeSignal};
use metrics::counter;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    config::SignalAgentConfig,
    math::{compute_expected_value, compute_kelly_fraction},
    state::{MarketBelief, SharedSignalState, new_shared_state},
};

// ---------------------------------------------------------------------------
// SignalAgent
// ---------------------------------------------------------------------------

/// Converts Bayesian posterior beliefs into sized trade signals.
///
/// ## Event subscriptions
/// - `Event::Posterior(PosteriorUpdate)` — stores belief, tries to emit signal
/// - `Event::Market(MarketUpdate)`       — updates market price, tries to emit signal
///
/// ## Signal pipeline (per update)
/// 1. Gate: market price within `[min_market_price, max_market_price]`
/// 2. Gate: `confidence >= min_confidence`
/// 3. Compute EV; reject if `EV < min_expected_value`
/// 4. Kelly sizing: clamp to `max_position_fraction`, scale by `kelly_fraction`
/// 5. Publish `Event::Signal(TradeSignal)`
///
/// ## Thread safety
/// `run()` consumes `self` and drives a single async task.  The shared state
/// is `Arc<RwLock<>>` and locks are never held across `.await` points.
pub struct SignalAgent {
    config: SignalAgentConfig,
    state: SharedSignalState,
    bus: EventBus,
    rx: Option<broadcast::Receiver<std::sync::Arc<Event>>>,
}

impl SignalAgent {
    /// Create a new agent with the given config and event bus.
    pub fn new(bus: EventBus, config: SignalAgentConfig) -> Self {
        let rx = bus.subscribe();
        Self {
            config,
            state: new_shared_state(),
            bus,
            rx: Some(rx),
        }
    }

    /// Clone the shared state handle for external inspection (e.g. health checks).
    pub fn state(&self) -> SharedSignalState {
        self.state.clone()
    }

    // -----------------------------------------------------------------------
    // Async event loop
    // -----------------------------------------------------------------------

    /// Run the agent until `cancel` is triggered or the bus closes.
    ///
    /// Consumes `self`; spawn as a Tokio task:
    /// ```ignore
    /// let cancel = CancellationToken::new();
    /// tokio::spawn(signal_agent.run(cancel.child_token()));
    /// ```
    pub async fn run(mut self, cancel: CancellationToken) {
        let mut rx = self.rx.take().expect("rx already consumed");
        info!("signal_agent: started");

        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    info!("signal_agent: cancelled, shutting down");
                    break;
                }
                result = rx.recv() => {
                    match result {
                        Ok(ev) => self.handle_event(ev.as_ref()).await,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("signal_agent: lagged by {n} events — consider increasing bus capacity");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("signal_agent: bus closed, shutting down");
                            break;
                        }
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Event dispatch
    // -----------------------------------------------------------------------

    async fn handle_event(&self, event: &Event) {
        match event {
            Event::Posterior(update) => {
                // 1. Store the belief.
                {
                    let mut state = self.state.write().await;
                    state.latest_beliefs.insert(
                        update.market_id.clone(),
                        MarketBelief {
                            posterior_prob: update.posterior_prob,
                            confidence: update.confidence,
                        },
                    );
                }

                // 2. If we already have a market price, attempt signal generation.
                //    Drop the write lock before the (potentially allocating) read.
                let market_price = {
                    let state = self.state.read().await;
                    state.latest_market_prices.get(&update.market_id).copied()
                };

                if let Some(price) = market_price {
                    self.try_emit_signal(
                        &update.market_id,
                        update.posterior_prob,
                        price,
                        update.confidence,
                    )
                    .await;
                }
            }

            Event::Market(update) => {
                let market_id = update.market.id.clone();
                let price = update.market.probability;

                // 1. Store the market price.
                {
                    let mut state = self.state.write().await;
                    state.latest_market_prices.insert(market_id.clone(), price);
                }

                // 2. If we already have a posterior, attempt signal generation.
                let belief = {
                    let state = self.state.read().await;
                    state.latest_beliefs.get(&market_id).cloned()
                };

                if let Some(b) = belief {
                    self.try_emit_signal(&market_id, b.posterior_prob, price, b.confidence)
                        .await;
                }
            }

            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // Signal generation pipeline
    // -----------------------------------------------------------------------

    /// Attempt to generate and publish a `TradeSignal` for `market_id`.
    ///
    /// All gates are checked in order; returns early (no signal) if any fails.
    /// Locks are acquired and released before each `.await`.
    async fn try_emit_signal(
        &self,
        market_id: &str,
        posterior_prob: f64,
        market_prob: f64,
        confidence: f64,
    ) {
        // Gate 1: price bounds — skip near-certain / near-impossible markets.
        if market_prob > self.config.max_market_price
            || market_prob < self.config.min_market_price
        {
            debug!(
                market_id,
                market_prob,
                "signal_agent: skipping — price out of bounds"
            );
            return;
        }

        // Gate 2: minimum model confidence.
        if confidence < self.config.min_confidence {
            debug!(
                market_id,
                confidence,
                "signal_agent: skipping — confidence below threshold"
            );
            return;
        }

        // Gate 3: Expected Value.
        let trading_cost = self.config.trading_cost();
        let Some((direction, ev)) =
            compute_expected_value(posterior_prob, market_prob, trading_cost)
        else {
            debug!(
                market_id,
                posterior_prob,
                market_prob,
                "signal_agent: no positive EV after trading costs"
            );
            return;
        };

        if ev < self.config.min_expected_value {
            debug!(
                market_id,
                ev,
                min = self.config.min_expected_value,
                "signal_agent: EV below minimum threshold"
            );
            return;
        }

        // Gate 4: Kelly position size.
        let position_fraction = compute_kelly_fraction(
            posterior_prob,
            market_prob,
            direction,
            self.config.max_position_fraction,
            self.config.kelly_fraction,
        );

        // Build and publish the signal.
        // `direction` is Copy so no clone needed.
        let signal = TradeSignal {
            market_id: market_id.to_string(),
            direction,
            expected_value: ev,
            position_fraction,
            posterior_prob,
            market_prob,
            confidence,
            timestamp: Utc::now(),
            source: "signal_agent".to_string(),
        };

        // Increment state counter under a single write lock acquisition.
        {
            let mut state = self.state.write().await;
            state.signals_emitted += 1;
        }

        counter!("signals_emitted_total").increment(1);
        match direction {
            common::TradeDirection::Buy => counter!("signals_buy_total").increment(1),
            common::TradeDirection::Sell => counter!("signals_sell_total").increment(1),
            _ => {}
        }

        info!(
            market_id,
            direction = ?signal.direction,
            ev = signal.expected_value,
            position_fraction = signal.position_fraction,
            posterior_prob,
            market_prob,
            confidence,
            "signal_agent: emitting TradeSignal"
        );

        if let Err(e) = self.bus.publish(Event::Signal(signal)) {
            warn!("signal_agent: failed to publish TradeSignal: {e}");
        }
    }
}
