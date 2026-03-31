// agents/temporal_agent/src/engine.rs
//
// Temporal Strategy Agent — async event loop.
//
// ## Detection approach
//
// For each market, a rolling window of recent prices is maintained.
// On every `MarketUpdate`, the z-score of the current price relative to its
// own history is computed:
//
//   mean   = mean(prices)
//   stddev = sample_stddev(prices)
//   z      = (current − mean) / stddev
//
// If |z| ≥ `trend_z_threshold`, a signal is emitted in the direction of
// the trend (Buy when z > 0, Sell when z < 0).
//
// ## Signal calibration
//
// Magnitude of the trend: `trend = current − mean`
// Extrapolated price (model probability): `(current + trend).clamp(ε, 1−ε)`
// Clamped deviation: `deviation = extrapolated − current`
// Expected value: `|deviation| − trading_cost`
// Confidence: `(|z| / trend_z_scale).clamp(0, 1)`
// Kelly position (Buy): `deviation / (1 − current)`, scaled and clamped.
// Kelly position (Sell): `|deviation| / current`, scaled and clamped.
//
// Note: EV and Kelly use `deviation` (post-clamp) rather than raw `trend` to
// stay consistent with the clamped `posterior_prob` in the emitted TradeSignal.

use std::sync::Arc;

use chrono::Utc;
use common::{Event, EventBus, MarketUpdate, TradeDirection, TradeSignal};
use metrics::counter;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    config::TemporalConfig,
    state::{new_shared_state, MarketHistory, SharedTemporalState},
};

pub struct TemporalAgent {
    pub config: TemporalConfig,
    /// Shared state — `pub` so tests can inspect.
    pub state: SharedTemporalState,
    bus: EventBus,
    rx: Option<broadcast::Receiver<Arc<Event>>>,
}

impl TemporalAgent {
    pub fn new(config: TemporalConfig, bus: EventBus) -> Result<Self, anyhow::Error> {
        if let Err(e) = config.validate() {
            return Err(anyhow::anyhow!("invalid TemporalConfig: {e}"));
        }
        let rx = bus.subscribe();
        Ok(Self {
            config,
            state: new_shared_state(),
            bus,
            rx: Some(rx),
        })
    }

    pub fn state(&self) -> SharedTemporalState {
        self.state.clone()
    }

    // -----------------------------------------------------------------------
    // Async event loop
    // -----------------------------------------------------------------------

    pub async fn run(mut self, cancel: CancellationToken) {
        let mut rx = self.rx.take().expect("rx already consumed");
        info!("temporal_agent: started");

        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    info!("temporal_agent: cancelled, shutting down");
                    break;
                }
                result = rx.recv() => {
                    match result {
                        Ok(ev) => self.handle_event(ev.as_ref()).await,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(
                                "temporal_agent: lagged by {n} events \
                                 — consider increasing bus capacity"
                            );
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("temporal_agent: bus closed, shutting down");
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
        if let Event::Market(update) = event {
            self.on_market_update(update).await;
        }
    }

    // -----------------------------------------------------------------------
    // MarketUpdate — update rolling history and check for trend signal
    // -----------------------------------------------------------------------

    async fn on_market_update(&self, update: &MarketUpdate) {
        counter!("temporal_agent_market_updates_total").increment(1);

        let new_prob = update.market.probability;
        if !new_prob.is_finite() || !(0.0..=1.0).contains(&new_prob) {
            warn!(
                market_id = %update.market.id,
                prob = new_prob,
                "temporal_agent: invalid probability, skipping"
            );
            return;
        }

        let window = self.config.history_window;
        let min_history = self.config.min_history;

        let signal = {
            let mut state = self.state.write().await;
            state.events_processed += 1;

            let history = state.histories.entry(update.market.id.clone()).or_default();

            // Evaluate signal against the existing history BEFORE including the
            // current price. This prevents the current observation from pulling
            // the mean toward itself and biasing the z-score toward zero.
            let signal = if history.prices.len() >= min_history {
                try_signal(&update.market.id, history, new_prob, &self.config)
            } else {
                None
            };

            history.push(new_prob, window);

            if signal.is_some() {
                state.signals_emitted += 1;
            }
            signal
        };
        // Lock released.

        if let Some(s) = signal {
            self.publish_signal(s);
        }
    }

    // -----------------------------------------------------------------------
    // Publishing
    // -----------------------------------------------------------------------

    fn publish_signal(&self, signal: TradeSignal) {
        counter!("temporal_agent_signals_emitted_total").increment(1);
        debug!(
            market_id = %signal.market_id,
            direction = ?signal.direction,
            ev        = signal.expected_value,
            confidence = signal.confidence,
            "temporal_agent: trend signal emitted"
        );
        if let Err(e) = self.bus.publish(Event::Signal(signal)) {
            warn!("temporal_agent: failed to publish signal: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Pure signal generation — no I/O or async
// ---------------------------------------------------------------------------

/// Attempt to generate a trend signal from the current price and its history.
///
/// Returns `Some(TradeSignal)` when the z-score of the current price relative
/// to its rolling mean exceeds `config.trend_z_threshold`.
fn try_signal(
    market_id: &str,
    history: &MarketHistory,
    current: f64,
    config: &TemporalConfig,
) -> Option<TradeSignal> {
    // Gate 1: Price bounds.
    if current < config.min_market_price || current > config.max_market_price {
        return None;
    }

    let mean = history.mean();
    let stddev = history.stddev()?;

    // Gate 2: Non-trivial variance required for z-scoring.
    if stddev < 1e-10 {
        return None;
    }

    let trend = current - mean; // positive → above average → bullish momentum
    let z = trend / stddev;

    // Gate 3: Z-score threshold.
    if z.abs() < config.trend_z_threshold {
        return None;
    }

    let direction = if z > 0.0 {
        TradeDirection::Buy
    } else {
        TradeDirection::Sell
    };

    // Model probability: extrapolate one step in the direction of the trend.
    // Clamp to a valid probability range away from 0 and 1.
    let extrapolated = (current + trend).clamp(0.01, 0.99);
    // Use the clamped deviation for EV and Kelly so they remain consistent
    // with the posterior_prob emitted in the TradeSignal.
    let deviation = extrapolated - current;

    // Gate 4: Expected value after trading costs.
    let ev = deviation.abs() - config.trading_cost;
    if ev <= 0.0 {
        return None;
    }

    // Confidence proportional to z-score strength.
    let confidence = (z.abs() / config.trend_z_scale).clamp(0.0, 1.0);

    // Kelly position sizing.
    let raw_kelly = match direction {
        TradeDirection::Buy => {
            let denom = 1.0 - current;
            if denom < 1e-12 {
                return None;
            }
            deviation / denom
        }
        TradeDirection::Sell => {
            if current < 1e-12 {
                return None;
            }
            deviation.abs() / current
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
        // Extrapolated price plays the role of "model probability".
        posterior_prob: extrapolated,
        market_prob: current,
        confidence,
        timestamp: Utc::now(),
        source: "temporal_agent".to_string(),
    })
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn config() -> TemporalConfig {
        TemporalConfig::default()
    }

    fn history_from(prices: &[f64]) -> MarketHistory {
        MarketHistory {
            prices: VecDeque::from(prices.to_vec()),
        }
    }

    /// Prices clustered tightly around `center` with amplitude `amp`.
    fn tight_history(n: usize, center: f64, amp: f64) -> Vec<f64> {
        (0..n)
            .map(|i| center + if i % 2 == 0 { amp } else { -amp })
            .collect()
    }

    #[test]
    fn no_signal_below_z_threshold() {
        // History tightly clustered at 0.50; current only slightly above.
        let prices = tight_history(20, 0.50, 0.01);
        let h = history_from(&prices);
        // A move of 0.01 with stddev≈0.01 → z≈1.0 < threshold 1.5 → no signal.
        let result = try_signal("M", &h, 0.51, &config());
        assert!(result.is_none(), "small move should not trigger signal");
    }

    #[test]
    fn buy_signal_on_large_upward_breakout() {
        // Very tight history (stddev≈0.001), then a big price jump.
        let prices = tight_history(20, 0.50, 0.001);
        let h = history_from(&prices);
        // current=0.70 → trend=0.20, z=0.20/0.001=200 >> 1.5
        let result = try_signal("M", &h, 0.70, &config());
        assert!(result.is_some(), "large upward move should trigger Buy");
        let s = result.unwrap();
        assert_eq!(s.direction, TradeDirection::Buy);
        assert!(s.expected_value > 0.0);
        assert!(s.confidence > 0.0 && s.confidence <= 1.0);
        assert!(s.position_fraction > 0.0 && s.position_fraction <= 0.03);
    }

    #[test]
    fn sell_signal_on_large_downward_break() {
        let prices = tight_history(20, 0.50, 0.001);
        let h = history_from(&prices);
        let result = try_signal("M", &h, 0.30, &config());
        assert!(result.is_some());
        assert_eq!(result.unwrap().direction, TradeDirection::Sell);
    }

    #[test]
    fn no_signal_on_flat_history() {
        // Identical prices → stddev = 0 → None.
        let prices = vec![0.5f64; 20];
        let h = history_from(&prices);
        assert!(try_signal("M", &h, 0.5, &config()).is_none());
    }

    #[test]
    fn confidence_capped_at_one() {
        let prices = tight_history(20, 0.50, 0.0001);
        let h = history_from(&prices);
        // Enormous z → confidence = 1.0
        let s = try_signal("M", &h, 0.90, &config()).unwrap();
        assert!((s.confidence - 1.0).abs() < 1e-9);
    }

    #[test]
    fn no_signal_outside_price_bounds() {
        let prices = tight_history(20, 0.97, 0.0001);
        let h = history_from(&prices);
        // current=0.98 > max_market_price=0.95 → suppressed
        assert!(try_signal("M", &h, 0.98, &config()).is_none());
    }
}
