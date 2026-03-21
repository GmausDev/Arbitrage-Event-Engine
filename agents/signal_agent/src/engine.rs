// agents/signal_agent/src/engine.rs
// Core Signal Agent: subscribes to PosteriorUpdate + MarketUpdate + MarketSnapshot
// events, computes Expected Value and Kelly position size, and emits TradeSignal events.

use std::collections::HashSet;
use std::time::Duration;

use chrono::Utc;
use common::{Event, EventBus, Timestamp, TradeSignal};
use metrics::counter;
use tokio::sync::broadcast;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    config::SignalAgentConfig,
    math::{compute_expected_value, compute_kelly_fraction, compute_liquidity_scale},
    state::{MarketBelief, SharedSignalState, new_shared_state},
};

// ---------------------------------------------------------------------------
// SignalAgent
// ---------------------------------------------------------------------------

/// Converts Bayesian posterior beliefs into sized trade signals.
///
/// ## Event subscriptions
/// - `Event::Posterior(PosteriorUpdate)`     — stores belief, tries to emit signal
/// - `Event::Market(MarketUpdate)`           — updates mid price + liquidity, tries signal
/// - `Event::MarketSnapshot(MarketSnapshot)` — stores bid/ask spread + liquidity
///
/// ## Signal pipeline (per update)
/// 1. Gate: market price within `[min_market_price, max_market_price]`
/// 2. Gate: `confidence >= min_confidence`
/// 3. Compute EV using bid/ask when available (fallback: mid price)
/// 4. Reject if `EV < min_expected_value`
/// 5. Kelly sizing with entry_price (ask/bid/mid)
/// 6. Liquidity scaling: reduce size proportionally for illiquid markets
/// 7. Publish `Event::Signal(TradeSignal)`
pub struct SignalAgent {
    config: SignalAgentConfig,
    state:  SharedSignalState,
    bus:    EventBus,
    rx:     Option<broadcast::Receiver<std::sync::Arc<Event>>>,
}

impl SignalAgent {
    pub fn new(bus: EventBus, config: SignalAgentConfig) -> Self {
        let rx = bus.subscribe();
        Self {
            config,
            state: new_shared_state(),
            bus,
            rx: Some(rx),
        }
    }

    pub fn state(&self) -> SharedSignalState {
        self.state.clone()
    }

    pub async fn run(mut self, cancel: CancellationToken) {
        let mut rx = self.rx.take().expect("rx already consumed");
        info!("signal_agent: started");

        // Evict stale market entries every 2 minutes.  Any market that has not
        // produced a Market or MarketSnapshot event within one eviction window is
        // no longer active and its cached state is freed.
        let mut eviction_tick = interval(Duration::from_secs(120));
        eviction_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut seen_ids: HashSet<String> = HashSet::new();

        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    info!("signal_agent: cancelled, shutting down");
                    break;
                }
                _ = eviction_tick.tick() => {
                    let mut st = self.state.write().await;
                    let before = st.latest_market_prices.len();
                    st.latest_market_prices.retain(|k, _| seen_ids.contains(k));
                    st.latest_liquidity.retain(|k, _| seen_ids.contains(k));
                    st.latest_spreads.retain(|k, _| seen_ids.contains(k));
                    st.latest_resolution_dates.retain(|k, _| seen_ids.contains(k));
                    st.latest_beliefs.retain(|k, _| seen_ids.contains(k));
                    let evicted = before.saturating_sub(st.latest_market_prices.len());
                    if evicted > 0 {
                        info!(evicted, remaining = st.latest_market_prices.len(),
                            "signal_agent: evicted stale market entries");
                    }
                    seen_ids.clear();
                }
                result = rx.recv() => {
                    match result {
                        Ok(ev) => {
                            // Track which markets are still active before dispatching.
                            match ev.as_ref() {
                                Event::Market(u)         => { seen_ids.insert(u.market.id.clone()); }
                                Event::MarketSnapshot(s) => { seen_ids.insert(s.market_id.clone()); }
                                _ => {}
                            }
                            self.handle_event(ev.as_ref()).await;
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("signal_agent: lagged by {n} events");
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
                {
                    let mut state = self.state.write().await;
                    state.latest_beliefs.insert(
                        update.market_id.clone(),
                        MarketBelief {
                            posterior_prob: update.posterior_prob,
                            confidence:     update.confidence,
                        },
                    );
                }

                let (market_price, bid, ask, liquidity, resolution_date) = {
                    let state = self.state.read().await;
                    let price     = state.latest_market_prices.get(&update.market_id).copied();
                    let spread    = state.latest_spreads.get(&update.market_id).copied();
                    let liquidity = state.latest_liquidity.get(&update.market_id).copied().unwrap_or(0.0);
                    let res_date  = state.latest_resolution_dates.get(&update.market_id).copied().flatten();
                    (price, spread.map(|(b, _)| b), spread.map(|(_, a)| a), liquidity, res_date)
                };

                if let Some(price) = market_price {
                    self.try_emit_signal(
                        &update.market_id,
                        update.posterior_prob,
                        price,
                        bid,
                        ask,
                        liquidity,
                        update.confidence,
                        resolution_date,
                    )
                    .await;
                }
            }

            Event::Market(update) => {
                let market_id = update.market.id.clone();
                let price     = update.market.probability;
                let liquidity = update.market.liquidity;

                {
                    let mut state = self.state.write().await;
                    state.latest_market_prices.insert(market_id.clone(), price);
                    // Only update liquidity from Market event if we don't have a
                    // fresher snapshot value (MarketSnapshot is preferred source).
                    state.latest_liquidity.entry(market_id.clone()).or_insert(liquidity);
                }

                let (belief, bid, ask, stored_liquidity, resolution_date) = {
                    let state = self.state.read().await;
                    let belief    = state.latest_beliefs.get(&market_id).cloned();
                    let spread    = state.latest_spreads.get(&market_id).copied();
                    let liq       = state.latest_liquidity.get(&market_id).copied().unwrap_or(liquidity);
                    let res_date  = state.latest_resolution_dates.get(&market_id).copied().flatten();
                    (belief, spread.map(|(b, _)| b), spread.map(|(_, a)| a), liq, res_date)
                };

                if let Some(b) = belief {
                    self.try_emit_signal(&market_id, b.posterior_prob, price, bid, ask, stored_liquidity, b.confidence, resolution_date)
                        .await;
                }
            }

            // MarketSnapshot carries bid/ask spread, authoritative liquidity, and resolution date.
            Event::MarketSnapshot(snap) => {
                let mut state = self.state.write().await;
                if snap.bid > 0.0 && snap.ask > snap.bid && snap.ask < 1.0 {
                    state.latest_spreads.insert(snap.market_id.clone(), (snap.bid, snap.ask));
                }
                // Snapshot liquidity is always authoritative.
                state.latest_liquidity.insert(snap.market_id.clone(), snap.liquidity);
                // Store resolution date for holding-period discount.
                state.latest_resolution_dates.insert(snap.market_id.clone(), snap.resolution_date);
            }

            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // Signal generation pipeline
    // -----------------------------------------------------------------------

    async fn try_emit_signal(
        &self,
        market_id:       &str,
        posterior_prob:  f64,
        market_prob:     f64,
        bid:             Option<f64>,
        ask:             Option<f64>,
        liquidity:       f64,
        confidence:      f64,
        resolution_date: Option<Timestamp>,
    ) {
        // Gate 1: price bounds.
        if market_prob > self.config.max_market_price
            || market_prob < self.config.min_market_price
        {
            debug!(market_id, market_prob, "signal_agent: price out of bounds");
            return;
        }

        // Gate 2: minimum model confidence.
        if confidence < self.config.min_confidence {
            debug!(market_id, confidence, "signal_agent: confidence below threshold");
            return;
        }

        // Gate 3: Expected Value (uses ask for BUY, bid for SELL when available).
        let trading_cost = self.config.trading_cost();
        let Some((direction, ev, entry_price)) =
            compute_expected_value(posterior_prob, market_prob, bid, ask, trading_cost)
        else {
            debug!(market_id, posterior_prob, market_prob, "signal_agent: no positive EV");
            return;
        };

        if ev < self.config.min_expected_value {
            debug!(market_id, ev, min = self.config.min_expected_value, "signal_agent: EV below threshold");
            return;
        }

        // Gate 4: Kelly position size (using real entry price, not mid).
        let mut position_fraction = compute_kelly_fraction(
            posterior_prob,
            entry_price,
            direction,
            self.config.max_position_fraction,
            self.config.kelly_fraction,
        );

        // Gate 5: Liquidity scaling.
        let liq_scale = compute_liquidity_scale(liquidity, self.config.bankroll, self.config.liquidity_threshold);
        position_fraction *= liq_scale;

        // Gate 5b: Holding-period (resolution) discount.
        // Markets resolving soon have higher per-day variance, so size them more
        // conservatively: position_fraction *= min(1.0, sqrt(days / 30)).
        // A market 30+ days away gets full size; a 7-day market gets ~48%.
        let time_scale = if let Some(res_date) = resolution_date {
            let now = Utc::now();
            let days = res_date.signed_duration_since(now).num_days().max(1) as f64;
            (days / 30.0_f64).sqrt().min(1.0)
        } else {
            1.0 // unknown expiry → no discount
        };
        position_fraction *= time_scale;

        // Gate 6: suppress zero-size signals (can happen when market is illiquid).
        if position_fraction <= 0.0 {
            debug!(market_id, liquidity, "signal_agent: liquidity too low — suppressing signal");
            return;
        }

        // Re-clamp after liquidity scaling (scaling can't push above max, but be safe).
        position_fraction = position_fraction.min(self.config.max_position_fraction);

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

        {
            let mut state = self.state.write().await;
            state.signals_emitted += 1;
        }

        counter!("signals_emitted_total").increment(1);
        match direction {
            common::TradeDirection::Buy  => counter!("signals_buy_total").increment(1),
            common::TradeDirection::Sell => counter!("signals_sell_total").increment(1),
            _ => {}
        }

        info!(
            market_id,
            direction         = ?signal.direction,
            ev                = signal.expected_value,
            entry_price,
            position_fraction = signal.position_fraction,
            liq_scale,
            time_scale,
            liquidity,
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
