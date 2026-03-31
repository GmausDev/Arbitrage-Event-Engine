// agents/bayesian_edge_agent/src/engine.rs
//
// Bayesian Edge Agent — async event loop.
//
// ## Pipeline position
//
// ```text
// bayesian_engine  ──→ Event::Posterior  ──→ BayesianEdgeAgent   ← this module
// market_scanner   ──→ Event::Market     ──→ BayesianEdgeAgent
// market_graph     ──→ Event::Graph      ──→ BayesianEdgeAgent   (optional damping)
// shock_detector   ──→ Event::Shock      ──→ BayesianEdgeAgent   (confidence boost)
//
// BayesianEdgeAgent ──→ Event::Signal  (to portfolio_optimizer)
// ```
//
// ## Detection summary
//
// Combines Bayesian posteriors with two optional fusion signals:
//
// **Graph damping** — if a graph-implied probability exists for the market,
//   the posterior is blended toward it:
//     posterior_adj = (1 − graph_damp) × posterior + graph_damp × implied
//   This damps the signal when the market-graph disagrees with the Bayesian model.
//
// **Shock boost** — if a fresh `InformationShock` exists for the market,
//   confidence is multiplied by (1 + shock_boost_factor × magnitude), capped at 1.0.
//   Only shocks within `shock_freshness_secs` seconds are counted.

use std::sync::Arc;

use chrono::Utc;
use common::{Event, EventBus, GraphUpdate, InformationShock, MarketUpdate, PosteriorUpdate};
use common::{TradeDirection, TradeSignal};
use metrics::counter;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    config::BayesianEdgeConfig,
    state::{new_shared_state, MarketBeliefEntry, SharedBayesianEdgeState},
};

pub struct BayesianEdgeAgent {
    pub config: BayesianEdgeConfig,
    /// Shared state — `pub` so tests can inspect.
    pub state: SharedBayesianEdgeState,
    bus: EventBus,
    rx: Option<broadcast::Receiver<Arc<Event>>>,
}

impl BayesianEdgeAgent {
    pub fn new(config: BayesianEdgeConfig, bus: EventBus) -> Result<Self, anyhow::Error> {
        if let Err(e) = config.validate() {
            return Err(anyhow::anyhow!("invalid BayesianEdgeConfig: {e}"));
        }
        let rx = bus.subscribe();
        Ok(Self {
            config,
            state: new_shared_state(),
            bus,
            rx: Some(rx),
        })
    }

    pub fn state(&self) -> SharedBayesianEdgeState {
        self.state.clone()
    }

    // -----------------------------------------------------------------------
    // Async event loop
    // -----------------------------------------------------------------------

    pub async fn run(mut self, cancel: CancellationToken) {
        let mut rx = self.rx.take().expect("rx already consumed");
        info!("bayesian_edge_agent: started");

        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    info!("bayesian_edge_agent: cancelled, shutting down");
                    break;
                }
                result = rx.recv() => {
                    match result {
                        Ok(ev) => self.handle_event(ev.as_ref()).await,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(
                                "bayesian_edge_agent: lagged by {n} events \
                                 — consider increasing bus capacity"
                            );
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("bayesian_edge_agent: bus closed, shutting down");
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
            Event::Posterior(update) => self.on_posterior_update(update).await,
            Event::Market(update) => self.on_market_update(update).await,
            Event::Graph(update) => self.on_graph_update(update).await,
            Event::Shock(shock) => self.on_shock(shock).await,
            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // PosteriorUpdate — update model probability and re-evaluate
    // -----------------------------------------------------------------------

    async fn on_posterior_update(&self, update: &PosteriorUpdate) {
        counter!("bayesian_edge_agent_posterior_updates_total").increment(1);

        if !update.posterior_prob.is_finite()
            || !(0.0..=1.0).contains(&update.posterior_prob)
            || !update.confidence.is_finite()
            || !(0.0..=1.0).contains(&update.confidence)
        {
            warn!(
                market_id = %update.market_id,
                "bayesian_edge_agent: invalid PosteriorUpdate fields, skipping"
            );
            return;
        }

        let signal = {
            let mut state = self.state.write().await;
            state.events_processed += 1;

            let entry = state.beliefs.entry(update.market_id.clone()).or_default();
            entry.posterior_prob = Some(update.posterior_prob);
            entry.posterior_confidence = Some(update.confidence);

            let signal = try_signal(&update.market_id, entry, &self.config, Utc::now());
            if signal.is_some() {
                state.signals_emitted += 1;
            }
            signal
        };

        if let Some(s) = signal {
            self.publish_signal(s);
        }
    }

    // -----------------------------------------------------------------------
    // MarketUpdate — refresh market price and re-evaluate
    // -----------------------------------------------------------------------

    async fn on_market_update(&self, update: &MarketUpdate) {
        counter!("bayesian_edge_agent_market_updates_total").increment(1);

        let new_prob = update.market.probability;
        if !new_prob.is_finite() || !(0.0..=1.0).contains(&new_prob) {
            warn!(
                market_id = %update.market.id,
                "bayesian_edge_agent: invalid market probability, skipping"
            );
            return;
        }

        let signal = {
            let mut state = self.state.write().await;
            state.events_processed += 1;

            let entry = state.beliefs.entry(update.market.id.clone()).or_default();
            entry.market_prob = Some(new_prob);

            let signal = try_signal(&update.market.id, entry, &self.config, Utc::now());
            if signal.is_some() {
                state.signals_emitted += 1;
            }
            signal
        };

        if let Some(s) = signal {
            self.publish_signal(s);
        }
    }

    // -----------------------------------------------------------------------
    // GraphUpdate — cache implied probabilities for damping
    // -----------------------------------------------------------------------

    async fn on_graph_update(&self, update: &GraphUpdate) {
        counter!("bayesian_edge_agent_graph_updates_total").increment(1);

        let mut state = self.state.write().await;
        state.events_processed += 1;

        for node in &update.node_updates {
            if !node.new_implied_prob.is_finite()
                || !(0.0..=1.0).contains(&node.new_implied_prob)
            {
                continue;
            }
            let entry = state.beliefs.entry(node.market_id.clone()).or_default();
            entry.implied_prob = Some(node.new_implied_prob);
        }
        // No signal emitted on graph updates alone — graph data only modifies
        // the damping applied to subsequent posterior/market updates.
    }

    // -----------------------------------------------------------------------
    // InformationShock — cache shock magnitude for confidence boosting
    // -----------------------------------------------------------------------

    async fn on_shock(&self, shock: &InformationShock) {
        counter!("bayesian_edge_agent_shocks_received_total").increment(1);

        let mut state = self.state.write().await;
        state.events_processed += 1;

        let entry = state.beliefs.entry(shock.market_id.clone()).or_default();
        entry.shock_magnitude = shock.magnitude.clamp(0.0, 1.0);
        entry.shock_ts = Some(shock.timestamp);
        // No immediate signal — the boost is applied on the next posterior/market update.
    }

    // -----------------------------------------------------------------------
    // Publishing
    // -----------------------------------------------------------------------

    fn publish_signal(&self, signal: TradeSignal) {
        counter!("bayesian_edge_agent_signals_emitted_total").increment(1);
        debug!(
            market_id  = %signal.market_id,
            direction  = ?signal.direction,
            ev         = signal.expected_value,
            confidence = signal.confidence,
            "bayesian_edge_agent: signal emitted"
        );
        if let Err(e) = self.bus.publish(Event::Signal(signal)) {
            warn!("bayesian_edge_agent: failed to publish signal: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Pure signal generation — no I/O or async
// ---------------------------------------------------------------------------

/// Attempt to generate a trade signal from the current belief state.
///
/// Returns `Some(TradeSignal)` when all gates pass; `None` otherwise.
fn try_signal(
    market_id: &str,
    entry: &MarketBeliefEntry,
    config: &BayesianEdgeConfig,
    now: chrono::DateTime<Utc>,
) -> Option<TradeSignal> {
    let posterior_prob = entry.posterior_prob?;
    let market_prob = entry.market_prob?;
    let base_confidence = entry.posterior_confidence?;

    // Gate 1: Price bounds.
    if market_prob < config.min_market_price || market_prob > config.max_market_price {
        return None;
    }

    // Gate 2: Shock-boosted confidence.
    let confidence = if let Some(shock_ts) = entry.shock_ts {
        let age = (now - shock_ts).num_seconds().unsigned_abs();
        if age <= config.shock_freshness_secs {
            let boost = 1.0 + config.shock_boost_factor * entry.shock_magnitude;
            (base_confidence * boost).clamp(0.0, 1.0)
        } else {
            base_confidence
        }
    } else {
        base_confidence
    };

    if confidence < config.min_confidence {
        return None;
    }

    // Graph damping: blend posterior toward graph-implied probability.
    let posterior_adj = if let Some(implied) = entry.implied_prob {
        let d = config.graph_damp_factor;
        (1.0 - d) * posterior_prob + d * implied
    } else {
        posterior_prob
    };

    // Gate 3: Deviation and direction.
    let deviation = posterior_adj - market_prob;
    let direction = if deviation > 0.0 {
        TradeDirection::Buy
    } else if deviation < 0.0 {
        TradeDirection::Sell
    } else {
        return None; // zero deviation → no edge
    };

    // Gate 4: Expected value after trading costs.
    let ev = deviation.abs() - config.trading_cost;
    if ev <= 0.0 || ev < config.min_expected_value {
        return None;
    }

    // Kelly position sizing.
    let raw_kelly = match direction {
        TradeDirection::Buy => {
            let denom = 1.0 - market_prob;
            if denom < 1e-12 {
                return None;
            }
            deviation / denom
        }
        TradeDirection::Sell => {
            if market_prob < 1e-12 {
                return None;
            }
            (-deviation) / market_prob
        }
        TradeDirection::Arbitrage => return None,
    };
    let position_fraction =
        (raw_kelly.max(0.0) * config.kelly_fraction).min(config.max_position_fraction);

    Some(TradeSignal {
        market_id: market_id.to_string(),
        direction,
        expected_value: ev,
        position_fraction,
        posterior_prob: posterior_adj,
        market_prob,
        confidence,
        timestamp: now,
        source: "bayesian_edge_agent".to_string(),
    })
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> BayesianEdgeConfig {
        BayesianEdgeConfig::default()
    }

    fn entry(
        posterior: Option<f64>,
        confidence: Option<f64>,
        market: Option<f64>,
    ) -> MarketBeliefEntry {
        MarketBeliefEntry {
            posterior_prob: posterior,
            posterior_confidence: confidence,
            market_prob: market,
            implied_prob: None,
            shock_magnitude: 0.0,
            shock_ts: None,
        }
    }

    #[test]
    fn no_signal_without_market_price() {
        let e = entry(Some(0.7), Some(0.8), None);
        assert!(try_signal("M", &e, &config(), Utc::now()).is_none());
    }

    #[test]
    fn no_signal_without_posterior() {
        let e = entry(None, None, Some(0.5));
        assert!(try_signal("M", &e, &config(), Utc::now()).is_none());
    }

    #[test]
    fn buy_signal_on_positive_deviation() {
        let e = entry(Some(0.70), Some(0.80), Some(0.50));
        let s = try_signal("M", &e, &config(), Utc::now()).unwrap();
        assert_eq!(s.direction, TradeDirection::Buy);
        assert!((s.expected_value - (0.20 - 0.003)).abs() < 1e-9);
    }

    #[test]
    fn sell_signal_on_negative_deviation() {
        let e = entry(Some(0.30), Some(0.80), Some(0.60));
        let s = try_signal("M", &e, &config(), Utc::now()).unwrap();
        assert_eq!(s.direction, TradeDirection::Sell);
    }

    #[test]
    fn low_confidence_suppressed() {
        // min_confidence = 0.25; confidence here = 0.10
        let e = entry(Some(0.70), Some(0.10), Some(0.50));
        assert!(try_signal("M", &e, &config(), Utc::now()).is_none());
    }

    #[test]
    fn shock_boosts_confidence_above_threshold() {
        // base_confidence=0.15 < min_confidence=0.25, but shock boosts to 0.15*1.5=0.225…
        // Actually: boost = 1 + 0.5 * 1.0 = 1.5, boosted = 0.15 * 1.5 = 0.225 < 0.25
        // Still below threshold. Use base=0.20: 0.20 * 1.5 = 0.30 > 0.25.
        let mut e = entry(Some(0.70), Some(0.20), Some(0.50));
        e.shock_magnitude = 1.0;
        e.shock_ts = Some(Utc::now());
        // boosted = 0.20 * 1.5 = 0.30 ≥ 0.25 → signal emitted
        let s = try_signal("M", &e, &config(), Utc::now());
        assert!(s.is_some(), "shock should boost confidence enough to emit");
    }

    #[test]
    fn stale_shock_does_not_boost() {
        let mut e = entry(Some(0.70), Some(0.20), Some(0.50));
        e.shock_magnitude = 1.0;
        // 600 s > shock_freshness_secs=300 → treated as absent
        e.shock_ts = Some(Utc::now() - chrono::Duration::seconds(600));
        // base_confidence=0.20 < min_confidence=0.25 → suppressed
        assert!(try_signal("M", &e, &config(), Utc::now()).is_none());
    }

    #[test]
    fn graph_damp_reduces_deviation() {
        // posterior=0.80, market=0.50, implied=0.55
        // posterior_adj = 0.8 * 0.80 + 0.2 * 0.55 = 0.64 + 0.11 = 0.75
        // deviation = 0.75 - 0.50 = 0.25 (less than raw 0.30)
        let mut e = entry(Some(0.80), Some(0.90), Some(0.50));
        e.implied_prob = Some(0.55);
        let s = try_signal("M", &e, &config(), Utc::now()).unwrap();
        assert!((s.expected_value - (0.25 - 0.003)).abs() < 1e-9);
    }
}
