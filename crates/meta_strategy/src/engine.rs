// crates/meta_strategy/src/engine.rs
//
// Meta Strategy Engine — fuses signals from all strategy agents.
//
// ## Pipeline position
//
// ```text
// signal_agent         ──→ Event::Signal ──→ MetaStrategyEngine   ← this module
// graph_arb_agent      ──→ Event::Signal ──→ MetaStrategyEngine
// temporal_agent       ──→ Event::Signal ──→ MetaStrategyEngine
// bayesian_edge_agent  ──→ Event::Signal ──→ MetaStrategyEngine
// shock_detector       ──→ Event::Shock  ──→ MetaStrategyEngine   (confidence boost)
//
// MetaStrategyEngine   ──→ Event::MetaSignal  (to portfolio_optimizer)
// ```
//
// ## Aggregation algorithm
//
// For each market, the engine maintains one `SignalEntry` per strategy source
// (keyed by `TradeSignal::source`).  Entries expire after `signal_ttl_secs`.
//
// On every new signal, all fresh entries for that market are aggregated:
//
//   1. **Direction voting** — each entry contributes a weight
//      `w = confidence × expected_value`.  The direction with the higher total
//      weight wins.
//
//   2. **Combined confidence** — directional conviction margin:
//      `combined_confidence = |buy_weight − sell_weight| / (buy_weight + sell_weight)`
//      (ranges 0 = perfect split → 1 = unanimous).
//
//   3. **Shock boost** — if a fresh `InformationShock` exists for the market,
//      `confidence *= (1 + shock_boost_factor × magnitude)`, capped at 1.0.
//
//   4. **Expected edge** — confidence-weighted mean EV of signals aligned with
//      the winning direction: `expected_edge = Σ(conf_i × ev_i) / Σ(conf_i)`
//      summed over winning-direction entries only.  Opposing signals are
//      excluded so they cannot dilute the reported edge.
//
//   5. **Gates** — `min_combined_confidence`, `min_expected_edge`, and
//      `min_strategies` must all be satisfied before a `MetaSignal` is emitted.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use common::{Event, EventBus, InformationShock, MetaSignal, TradeDirection, TradeSignal};
use metrics::counter;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    config::MetaStrategyConfig,
    state::{new_shared_state, MarketMeta, SharedMetaStrategyState, SignalEntry},
};

// ---------------------------------------------------------------------------
// MetaStrategyEngine
// ---------------------------------------------------------------------------

pub struct MetaStrategyEngine {
    pub config: MetaStrategyConfig,
    /// Shared state — `pub` so tests and health checks can inspect it.
    pub state: SharedMetaStrategyState,
    bus: EventBus,
    rx: Option<broadcast::Receiver<Arc<Event>>>,
}

impl MetaStrategyEngine {
    pub fn new(config: MetaStrategyConfig, bus: EventBus) -> Self {
        if let Err(e) = config.validate() {
            panic!("invalid MetaStrategyConfig: {e}");
        }
        let rx = bus.subscribe();
        Self {
            config,
            state: new_shared_state(),
            bus,
            rx: Some(rx),
        }
    }

    /// Clone the shared state handle for external inspection.
    pub fn state(&self) -> SharedMetaStrategyState {
        self.state.clone()
    }

    // -----------------------------------------------------------------------
    // Async event loop
    // -----------------------------------------------------------------------

    pub async fn run(mut self, cancel: CancellationToken) {
        let mut rx = self.rx.take().expect("rx already consumed");
        info!("meta_strategy: started");

        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    info!("meta_strategy: cancelled, shutting down");
                    break;
                }
                result = rx.recv() => {
                    match result {
                        Ok(ev) => self.handle_event(ev.as_ref()).await,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(
                                "meta_strategy: lagged by {n} events \
                                 — consider increasing bus capacity"
                            );
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("meta_strategy: bus closed, shutting down");
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
            Event::Signal(signal) => self.on_signal(signal).await,
            Event::Shock(shock)   => self.on_shock(shock).await,
            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // Signal ingestion — store, aggregate, conditionally publish
    // -----------------------------------------------------------------------

    async fn on_signal(&self, signal: &TradeSignal) {
        counter!("meta_strategy_signals_received_total").increment(1);

        // Validate the incoming signal's key fields.
        if !signal.confidence.is_finite()
            || !signal.expected_value.is_finite()
            || signal.expected_value <= 0.0
        {
            warn!(
                market_id = %signal.market_id,
                "meta_strategy: invalid signal fields, skipping"
            );
            return;
        }

        let source = if signal.source.is_empty() {
            // Assign a synthetic key so blank-source signals still participate.
            format!("unknown_{}", signal.market_id)
        } else {
            signal.source.clone()
        };

        let meta_signal = {
            let mut state = self.state.write().await;
            state.events_processed += 1;

            let market_meta = state.markets.entry(signal.market_id.clone()).or_default();

            // Store (overwriting any prior entry from the same strategy).
            market_meta.entries.insert(
                source,
                SignalEntry {
                    signal: signal.clone(),
                    received_at: Utc::now(),
                },
            );

            let meta =
                try_meta_signal(&signal.market_id, market_meta, &self.config, Utc::now());
            if meta.is_some() {
                state.signals_emitted += 1;
            }
            meta
        };
        // Lock released — safe to publish.

        if let Some(ms) = meta_signal {
            self.publish_meta_signal(ms);
        }
    }

    // -----------------------------------------------------------------------
    // Shock ingestion — cache magnitude for confidence boosting
    // -----------------------------------------------------------------------

    async fn on_shock(&self, shock: &InformationShock) {
        counter!("meta_strategy_shocks_received_total").increment(1);

        let mut state = self.state.write().await;
        state.events_processed += 1;

        let market_meta = state.markets.entry(shock.market_id.clone()).or_default();
        market_meta.shock_magnitude = shock.magnitude.clamp(0.0, 1.0);
        market_meta.shock_ts = Some(shock.timestamp);
        // No immediate MetaSignal on shock alone — the boost is applied on the
        // next Signal event for the same market.
    }

    // -----------------------------------------------------------------------
    // Publishing
    // -----------------------------------------------------------------------

    fn publish_meta_signal(&self, ms: MetaSignal) {
        counter!("meta_strategy_signals_emitted_total").increment(1);
        debug!(
            market_id  = %ms.market_id,
            direction  = ?ms.direction,
            confidence = ms.confidence,
            edge       = ms.expected_edge,
            strategies = ?ms.contributing_strategies,
            "meta_strategy: MetaSignal emitted"
        );
        if let Err(e) = self.bus.publish(Event::MetaSignal(ms)) {
            warn!("meta_strategy: failed to publish MetaSignal: {e}");
        }
    }

    // -----------------------------------------------------------------------
    // Query API — top N signals by risk-adjusted EV (confidence × edge)
    // -----------------------------------------------------------------------

    /// Compute meta signals for every tracked market and return the top `n`
    /// ranked by `confidence × expected_edge` (risk-adjusted EV).
    ///
    /// This is a read-only operation; no state is mutated and no events are
    /// published.
    pub async fn top_n_signals(&self, n: usize) -> Vec<MetaSignal> {
        let now = Utc::now();
        let state = self.state.read().await;

        let mut signals: Vec<MetaSignal> = state
            .markets
            .iter()
            .filter_map(|(market_id, meta)| {
                try_meta_signal(market_id, meta, &self.config, now)
            })
            .collect();

        // Sort descending by risk-adjusted EV.
        signals.sort_by(|a, b| {
            let raev_a = a.confidence * a.expected_edge;
            let raev_b = b.confidence * b.expected_edge;
            raev_b.partial_cmp(&raev_a).unwrap_or(std::cmp::Ordering::Equal)
        });

        signals.truncate(n);
        signals
    }
}

// ---------------------------------------------------------------------------
// Pure aggregation — no I/O or async
// ---------------------------------------------------------------------------

/// Attempt to generate a `MetaSignal` from the current per-market aggregation.
///
/// Returns `Some(MetaSignal)` when all gates pass; `None` otherwise.
pub(crate) fn try_meta_signal(
    market_id: &str,
    meta: &MarketMeta,
    config: &MetaStrategyConfig,
    now: DateTime<Utc>,
) -> Option<MetaSignal> {
    let ttl = chrono::Duration::seconds(config.signal_ttl_secs as i64);

    // ── 1. Collect fresh entries ────────────────────────────────────────────
    let fresh: Vec<&SignalEntry> = meta
        .entries
        .values()
        .filter(|e| now - e.received_at <= ttl)
        .collect();

    if fresh.len() < config.min_strategies {
        return None;
    }

    // ── 2. Direction voting (weighted by confidence × EV) ──────────────────
    // Arbitrage signals have no directional conviction and are excluded from
    // the vote (they still count toward min_strategies).
    let mut buy_weight  = 0.0f64;
    let mut sell_weight = 0.0f64;

    for entry in &fresh {
        let w = entry.signal.confidence * entry.signal.expected_value;
        match entry.signal.direction {
            TradeDirection::Buy       => buy_weight  += w,
            TradeDirection::Sell      => sell_weight += w,
            TradeDirection::Arbitrage => {}  // excluded — no directional vote
        }
    }

    // Need at least some directional weight to proceed.
    let total = buy_weight + sell_weight;
    if total < 1e-12 {
        return None;
    }

    let direction = if buy_weight >= sell_weight {
        TradeDirection::Buy
    } else {
        TradeDirection::Sell
    };

    // ── 3. Combined confidence = directional vote margin ───────────────────
    let net = (buy_weight - sell_weight).abs();
    let combined_confidence = (net / total).clamp(0.0, 1.0);

    // ── 4. Shock boost ─────────────────────────────────────────────────────
    let confidence = if let Some(shock_ts) = meta.shock_ts {
        let age_secs = (now - shock_ts).num_seconds().unsigned_abs();
        if age_secs <= config.shock_freshness_secs {
            let boost = 1.0 + config.shock_boost_factor * meta.shock_magnitude;
            (combined_confidence * boost).clamp(0.0, 1.0)
        } else {
            combined_confidence
        }
    } else {
        combined_confidence
    };

    // ── Gate A: minimum directional conviction ─────────────────────────────
    if confidence < config.min_combined_confidence {
        return None;
    }

    // ── 5. Expected edge — confidence-weighted mean EV of winning-side signals
    // Only entries whose direction matches the winning direction are included.
    // Opposing signals must not dilute the edge reported to downstream
    // consumers (portfolio_optimizer, risk_engine).
    let mut aligned_conf      = 0.0f64;
    let mut aligned_ev_weight = 0.0f64;
    for entry in &fresh {
        if entry.signal.direction == direction {
            aligned_conf      += entry.signal.confidence;
            aligned_ev_weight += entry.signal.confidence * entry.signal.expected_value;
        }
    }
    let expected_edge = if aligned_conf > 1e-12 {
        aligned_ev_weight / aligned_conf
    } else {
        0.0
    };

    // ── Gate B: minimum edge ───────────────────────────────────────────────
    if expected_edge < config.min_expected_edge {
        return None;
    }

    // ── 6. Contributing strategies (sorted for determinism) ────────────────
    let mut strategies: Vec<String> = fresh
        .iter()
        .map(|e| e.signal.source.clone())
        .filter(|s| !s.is_empty())
        .collect();
    strategies.sort();
    strategies.dedup();

    Some(MetaSignal {
        market_id: market_id.to_string(),
        direction,
        confidence,
        expected_edge,
        contributing_strategies: strategies,
        signal_count: fresh.len(),
        timestamp: now,
    })
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use common::TradeDirection;

    fn config() -> MetaStrategyConfig {
        MetaStrategyConfig::default()
    }

    /// Build a `MarketMeta` with exactly the given entries (no shock).
    fn meta_from(entries: Vec<(&str, TradeDirection, f64, f64)>) -> MarketMeta {
        let mut market_meta = MarketMeta::default();
        for (source, direction, confidence, ev) in entries {
            market_meta.entries.insert(
                source.to_string(),
                SignalEntry {
                    signal: TradeSignal {
                        market_id:         "M".to_string(),
                        direction,
                        expected_value:    ev,
                        position_fraction: 0.05,
                        posterior_prob:    0.70,
                        market_prob:       0.50,
                        confidence,
                        timestamp:         Utc::now(),
                        source:            source.to_string(),
                    },
                    received_at: Utc::now(),
                },
            );
        }
        market_meta
    }

    // ── 1. No fresh entries → None ─────────────────────────────────────────

    #[test]
    fn no_signal_with_no_entries() {
        let meta = MarketMeta::default();
        assert!(try_meta_signal("M", &meta, &config(), Utc::now()).is_none());
    }

    // ── 2. Single strong Buy signal → MetaSignal(Buy) ─────────────────────

    #[test]
    fn single_buy_signal_emits_meta_signal() {
        // weight = 0.8 × 0.10 = 0.08; combined_confidence = 0.08/0.08 = 1.0
        let meta = meta_from(vec![("bayesian_edge_agent", TradeDirection::Buy, 0.80, 0.10)]);
        let ms = try_meta_signal("M", &meta, &config(), Utc::now()).unwrap();
        assert_eq!(ms.direction, TradeDirection::Buy);
        assert!((ms.confidence - 1.0).abs() < 1e-9);
        assert!((ms.expected_edge - 0.10).abs() < 1e-9);
        assert_eq!(ms.contributing_strategies, vec!["bayesian_edge_agent"]);
        assert_eq!(ms.signal_count, 1);
    }

    // ── 3. Single strong Sell signal ──────────────────────────────────────

    #[test]
    fn single_sell_signal_emits_meta_signal() {
        let meta = meta_from(vec![("temporal_agent", TradeDirection::Sell, 0.75, 0.12)]);
        let ms = try_meta_signal("M", &meta, &config(), Utc::now()).unwrap();
        assert_eq!(ms.direction, TradeDirection::Sell);
        assert!((ms.confidence - 1.0).abs() < 1e-9);
    }

    // ── 4. Buy wins conflict by weight margin ──────────────────────────────

    #[test]
    fn buy_wins_conflict_by_weight() {
        // Buy: 0.8 × 0.12 = 0.096,  Sell: 0.4 × 0.06 = 0.024
        // net = 0.072, total = 0.120, combined_confidence = 0.60
        let meta = meta_from(vec![
            ("bayesian_edge_agent", TradeDirection::Buy,  0.80, 0.12),
            ("temporal_agent",      TradeDirection::Sell, 0.40, 0.06),
        ]);
        let ms = try_meta_signal("M", &meta, &config(), Utc::now()).unwrap();
        assert_eq!(ms.direction, TradeDirection::Buy);
        assert!((ms.confidence - 0.60).abs() < 1e-9);
        assert_eq!(ms.signal_count, 2);
    }

    // ── 5. Sell wins conflict by weight margin ─────────────────────────────

    #[test]
    fn sell_wins_conflict_by_weight() {
        // Buy: 0.3 × 0.05 = 0.015,  Sell: 0.9 × 0.20 = 0.180
        // net = 0.165, total = 0.195, combined_confidence = 0.846...
        let meta = meta_from(vec![
            ("graph_arb_agent", TradeDirection::Buy,  0.30, 0.05),
            ("signal_agent",    TradeDirection::Sell, 0.90, 0.20),
        ]);
        let ms = try_meta_signal("M", &meta, &config(), Utc::now()).unwrap();
        assert_eq!(ms.direction, TradeDirection::Sell);
        assert!(ms.confidence > 0.80);
    }

    // ── 6. Perfect conflict suppressed ────────────────────────────────────

    #[test]
    fn perfect_conflict_suppressed() {
        // Both sides have identical weights → net = 0 → combined_confidence = 0 < 0.30
        let meta = meta_from(vec![
            ("strat_a", TradeDirection::Buy,  0.50, 0.10),
            ("strat_b", TradeDirection::Sell, 0.50, 0.10),
        ]);
        assert!(try_meta_signal("M", &meta, &config(), Utc::now()).is_none());
    }

    // ── 7. Shock boost crosses confidence threshold ────────────────────────

    #[test]
    fn shock_boost_crosses_threshold() {
        // Buy: 0.5 × 0.10 = 0.050,  Sell: 0.6 × 0.05 = 0.030
        // net = 0.020, total = 0.080, combined_confidence = 0.25 < 0.30
        let mut meta = meta_from(vec![
            ("strat_buy",  TradeDirection::Buy,  0.50, 0.10),
            ("strat_sell", TradeDirection::Sell, 0.60, 0.05),
        ]);
        // Without shock: suppressed.
        assert!(try_meta_signal("M", &meta, &config(), Utc::now()).is_none());

        // Add fresh shock (magnitude=1.0): boosted = 0.25 × (1 + 0.25×1.0) = 0.3125 ≥ 0.30
        meta.shock_magnitude = 1.0;
        meta.shock_ts        = Some(Utc::now());
        let ms = try_meta_signal("M", &meta, &config(), Utc::now()).unwrap();
        assert_eq!(ms.direction, TradeDirection::Buy);
        assert!(ms.confidence >= 0.30);
    }

    // ── 8. Stale shock provides no boost ──────────────────────────────────

    #[test]
    fn stale_shock_does_not_boost() {
        let mut meta = meta_from(vec![
            ("strat_buy",  TradeDirection::Buy,  0.50, 0.10),
            ("strat_sell", TradeDirection::Sell, 0.60, 0.05),
        ]);
        // Shock older than shock_freshness_secs=300 → treated as absent.
        meta.shock_magnitude = 1.0;
        meta.shock_ts        = Some(Utc::now() - Duration::seconds(600));
        assert!(try_meta_signal("M", &meta, &config(), Utc::now()).is_none());
    }

    // ── 9. Stale signals excluded by TTL ──────────────────────────────────

    #[test]
    fn stale_signals_excluded_by_ttl() {
        let mut meta = MarketMeta::default();

        // One very stale entry (65 s old — beyond default TTL of 60 s).
        meta.entries.insert(
            "old_strat".to_string(),
            SignalEntry {
                signal: TradeSignal {
                    market_id: "M".to_string(),
                    direction: TradeDirection::Buy,
                    expected_value: 0.20,
                    position_fraction: 0.05,
                    posterior_prob: 0.80,
                    market_prob:    0.50,
                    confidence:     0.90,
                    timestamp:      Utc::now(),
                    source:         "old_strat".to_string(),
                },
                received_at: Utc::now() - Duration::seconds(65),
            },
        );

        // min_strategies=1 but the only entry is stale → fresh count = 0 < 1 → None.
        assert!(try_meta_signal("M", &meta, &config(), Utc::now()).is_none());
    }

    // ── 10. min_strategies=2 with only one strategy → suppressed ──────────

    #[test]
    fn min_strategies_two_requires_corroboration() {
        let cfg = MetaStrategyConfig {
            min_strategies: 2,
            ..MetaStrategyConfig::default()
        };
        let meta = meta_from(vec![("bayesian_edge_agent", TradeDirection::Buy, 0.90, 0.15)]);
        assert!(
            try_meta_signal("M", &meta, &cfg, Utc::now()).is_none(),
            "single strategy should be suppressed when min_strategies=2"
        );
    }

    // ── 11. min_strategies=2 with two strategies → passes ─────────────────

    #[test]
    fn min_strategies_two_passes_with_two_sources() {
        let cfg = MetaStrategyConfig {
            min_strategies: 2,
            ..MetaStrategyConfig::default()
        };
        let meta = meta_from(vec![
            ("bayesian_edge_agent", TradeDirection::Buy, 0.80, 0.12),
            ("graph_arb_agent",     TradeDirection::Buy, 0.70, 0.10),
        ]);
        let ms = try_meta_signal("M", &meta, &cfg, Utc::now()).unwrap();
        assert_eq!(ms.direction, TradeDirection::Buy);
        assert_eq!(ms.contributing_strategies.len(), 2);
    }

    // ── 12. Multiple same-direction signals → combined_confidence = 1.0 ───

    #[test]
    fn unanimous_buy_gives_full_confidence() {
        let meta = meta_from(vec![
            ("strat_a", TradeDirection::Buy, 0.80, 0.12),
            ("strat_b", TradeDirection::Buy, 0.60, 0.08),
            ("strat_c", TradeDirection::Buy, 0.70, 0.10),
        ]);
        let ms = try_meta_signal("M", &meta, &config(), Utc::now()).unwrap();
        assert_eq!(ms.direction, TradeDirection::Buy);
        // All Buy → net = total → combined_confidence = 1.0
        assert!((ms.confidence - 1.0).abs() < 1e-9);
        assert_eq!(ms.signal_count, 3);
    }

    // ── 13. Arbitrage signal excluded from direction vote ──────────────────
    //
    // An Arbitrage signal alongside a strong Buy should not dilute the
    // directional confidence (old 50/50 split inflated the denominator).
    // The Arbitrage entry still counts toward min_strategies.

    #[test]
    fn arbitrage_signal_excluded_from_direction_vote() {
        let mut meta = MarketMeta::default();

        // Strong Buy: w = 0.80 × 0.12 = 0.096 → combined_confidence = 1.0
        meta.entries.insert(
            "buy_strat".to_string(),
            SignalEntry {
                signal: TradeSignal {
                    market_id:         "M".to_string(),
                    direction:         TradeDirection::Buy,
                    expected_value:    0.12,
                    position_fraction: 0.05,
                    posterior_prob:    0.70,
                    market_prob:       0.50,
                    confidence:        0.80,
                    timestamp:         Utc::now(),
                    source:            "buy_strat".to_string(),
                },
                received_at: Utc::now(),
            },
        );
        // Arbitrage signal: should not appear in direction vote at all.
        meta.entries.insert(
            "arb_strat".to_string(),
            SignalEntry {
                signal: TradeSignal {
                    market_id:         "M".to_string(),
                    direction:         TradeDirection::Arbitrage,
                    expected_value:    0.05,
                    position_fraction: 0.02,
                    posterior_prob:    0.60,
                    market_prob:       0.55,
                    confidence:        0.70,
                    timestamp:         Utc::now(),
                    source:            "arb_strat".to_string(),
                },
                received_at: Utc::now(),
            },
        );

        let ms = try_meta_signal("M", &meta, &config(), Utc::now()).unwrap();
        // Direction should be Buy with full conviction (arbitrage excluded).
        assert_eq!(ms.direction, TradeDirection::Buy);
        assert!((ms.confidence - 1.0).abs() < 1e-9,
            "expected combined_confidence = 1.0, got {}", ms.confidence);
        // signal_count includes the arb entry (it counted toward min_strategies).
        assert_eq!(ms.signal_count, 2);
        // expected_edge should only reflect the Buy signal's EV.
        assert!((ms.expected_edge - 0.12).abs() < 1e-9,
            "expected_edge should be 0.12 (Buy only), got {}", ms.expected_edge);
    }

    // ── 15. expected_edge below threshold → suppressed ─────────────────────

    #[test]
    fn expected_edge_below_threshold_suppressed() {
        let cfg = MetaStrategyConfig {
            min_expected_edge: 0.10,
            ..MetaStrategyConfig::default()
        };
        // EV = 0.05 < 0.10 threshold.
        let meta = meta_from(vec![("strat_a", TradeDirection::Buy, 0.90, 0.05)]);
        assert!(try_meta_signal("M", &meta, &cfg, Utc::now()).is_none());
    }
}
