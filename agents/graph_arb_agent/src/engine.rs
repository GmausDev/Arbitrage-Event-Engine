// agents/graph_arb_agent/src/engine.rs
//
// Graph Arbitrage Agent — async event loop.
//
// ## Pipeline position
//
// ```text
// market_graph  ──→ Event::Graph ──→ GraphArbAgent   ← this module
// market_scanner ─→ Event::Market ──→ GraphArbAgent
//
// GraphArbAgent ──→ Event::Signal  (to portfolio_optimizer)
// ```
//
// ## Detection summary
//
// On every `GraphUpdate`, the BFS-propagated implied probability for each
// updated node is compared against the cached market price.  When
// |implied − market| exceeds `min_edge_threshold` and the net EV exceeds
// trading costs, a `TradeSignal` is emitted.
//
// ## Lock discipline
//
// Write-lock held only for state mutation; released before every publish.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use common::{
    Event, EventBus, GraphUpdate, MarketUpdate, MarketSnapshot, TradeDirection, TradeSignal,
};
use metrics::counter;
use tokio::sync::broadcast;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    config::GraphArbConfig,
    state::{new_shared_state, MarketPriceEntry, SharedGraphArbState},
    xplatform::{find_pairs_for, try_xplatform_signals},
};

pub struct GraphArbAgent {
    pub config: GraphArbConfig,
    /// Shared state — `pub` so tests can inspect.
    pub state: SharedGraphArbState,
    bus: EventBus,
    rx: Option<broadcast::Receiver<Arc<Event>>>,
}

impl GraphArbAgent {
    pub fn new(config: GraphArbConfig, bus: EventBus) -> Self {
        if let Err(e) = config.validate() {
            panic!("invalid GraphArbConfig: {e}");
        }
        let rx = bus.subscribe();
        Self {
            config,
            state: new_shared_state(),
            bus,
            rx: Some(rx),
        }
    }

    pub fn state(&self) -> SharedGraphArbState {
        self.state.clone()
    }

    // -----------------------------------------------------------------------
    // Async event loop
    // -----------------------------------------------------------------------

    pub async fn run(mut self, cancel: CancellationToken) {
        let mut rx = self.rx.take().expect("rx already consumed");
        info!("graph_arb_agent: started");

        // Evict stale state every 2 minutes:
        //   - `snapshots` and `markets`: retain only IDs seen in recent Market /
        //     MarketSnapshot events (seen-set pattern).
        //   - `xplatform_last_emitted`: retain pairs whose cooldown timestamp is
        //     still within 2 × xplatform_signal_cooldown_secs.  Older entries will
        //     never suppress a signal again and are safe to drop.
        let eviction_interval_secs = self.config.xplatform_signal_cooldown_secs * 2;
        let mut eviction_tick = interval(Duration::from_secs(120));
        eviction_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut seen_ids: HashSet<String> = HashSet::new();

        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    info!("graph_arb_agent: cancelled, shutting down");
                    break;
                }
                _ = eviction_tick.tick() => {
                    let cutoff = Utc::now()
                        - chrono::Duration::seconds(eviction_interval_secs as i64);
                    let mut state = self.state.write().await;

                    let snap_before = state.snapshots.len();
                    state.snapshots.retain(|k, _| seen_ids.contains(k));
                    state.markets.retain(|k, _| seen_ids.contains(k));

                    let xp_before = state.xplatform_last_emitted.len();
                    state.xplatform_last_emitted.retain(|_, ts| *ts >= cutoff);

                    let snaps_evicted = snap_before.saturating_sub(state.snapshots.len());
                    let xp_evicted = xp_before.saturating_sub(state.xplatform_last_emitted.len());
                    if snaps_evicted > 0 || xp_evicted > 0 {
                        info!(snaps_evicted, xp_evicted,
                            "graph_arb_agent: evicted stale entries");
                    }
                    seen_ids.clear();
                }
                result = rx.recv() => {
                    match result {
                        Ok(ev) => {
                            // Track active market IDs before dispatching.
                            match ev.as_ref() {
                                Event::Market(u)         => { seen_ids.insert(u.market.id.clone()); }
                                Event::MarketSnapshot(s) => { seen_ids.insert(s.market_id.clone()); }
                                _ => {}
                            }
                            self.handle_event(ev.as_ref()).await;
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(
                                "graph_arb_agent: lagged by {n} events \
                                 — consider increasing bus capacity"
                            );
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("graph_arb_agent: bus closed, shutting down");
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
            Event::Graph(update) => self.on_graph_update(update).await,
            Event::Market(update) => self.on_market_update(update).await,
            Event::MarketSnapshot(snap) => self.on_market_snapshot(snap).await,
            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // GraphUpdate — check every updated node for arbitrage
    // -----------------------------------------------------------------------

    async fn on_graph_update(&self, update: &GraphUpdate) {
        counter!("graph_arb_agent_graph_updates_total").increment(1);

        let signals = {
            let mut state = self.state.write().await;
            state.events_processed += 1;

            let mut collected = Vec::new();
            for node in &update.node_updates {
                if !node.new_implied_prob.is_finite()
                    || !(0.0..=1.0).contains(&node.new_implied_prob)
                {
                    warn!(
                        market_id = %node.market_id,
                        "graph_arb_agent: invalid implied_prob in GraphUpdate, skipping node"
                    );
                    continue;
                }

                let entry = state.markets.entry(node.market_id.clone()).or_default();
                entry.implied_prob = Some(node.new_implied_prob);

                if let Some(signal) = try_signal(&node.market_id, entry, &self.config) {
                    state.signals_emitted += 1;
                    collected.push(signal);
                }
            }
            collected
        };
        // Lock released — safe to publish.

        for signal in signals {
            self.publish_signal(signal);
        }
    }

    // -----------------------------------------------------------------------
    // MarketUpdate — refresh cached market price, re-check arb
    // -----------------------------------------------------------------------

    async fn on_market_update(&self, update: &MarketUpdate) {
        counter!("graph_arb_agent_market_updates_total").increment(1);

        let new_prob = update.market.probability;
        if !new_prob.is_finite() || !(0.0..=1.0).contains(&new_prob) {
            warn!(
                market_id = %update.market.id,
                prob = new_prob,
                "graph_arb_agent: invalid probability in MarketUpdate, skipping"
            );
            return;
        }

        let signal = {
            let mut state = self.state.write().await;
            state.events_processed += 1;

            let entry = state.markets.entry(update.market.id.clone()).or_default();
            entry.market_prob = Some(new_prob);

            let signal = try_signal(&update.market.id, entry, &self.config);
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
    // MarketSnapshot — cross-platform arbitrage detection
    // -----------------------------------------------------------------------

    async fn on_market_snapshot(&self, snap: &MarketSnapshot) {
        counter!("graph_arb_agent_market_snapshots_total").increment(1);

        // Ignore snapshots with no platform tag — we cannot form cross-platform
        // pairs without knowing which side each market belongs to.
        if snap.source_platform.is_empty() {
            return;
        }

        let now = Utc::now();

        let signals = {
            let mut state = self.state.write().await;
            state.events_processed += 1;

            // Update our snapshot cache.
            state.snapshots.insert(snap.market_id.clone(), snap.clone());

            // Find all cross-platform pairs involving this snapshot.
            let pairs = find_pairs_for(
                &snap.market_id,
                &state.snapshots,
                self.config.xplatform_min_title_similarity,
            );

            let mut collected: Vec<TradeSignal> = Vec::new();
            for pair in &pairs {
                let new_signals = try_xplatform_signals(
                    pair,
                    &self.config,
                    &state.xplatform_last_emitted,
                    now,
                );
                if !new_signals.is_empty() {
                    // Update cooldown timestamp before releasing the lock.
                    state
                        .xplatform_last_emitted
                        .insert(pair.cooldown_key(), now);
                    state.signals_emitted += new_signals.len() as u64;
                    state.xplatform_signals_emitted += new_signals.len() as u64;
                    collected.extend(new_signals);
                }
            }
            collected
        };
        // Lock released — safe to publish.

        for signal in signals {
            self.publish_signal(signal);
        }
    }

    // -----------------------------------------------------------------------
    // Publishing
    // -----------------------------------------------------------------------

    fn publish_signal(&self, signal: TradeSignal) {
        counter!("graph_arb_agent_signals_emitted_total").increment(1);
        debug!(
            market_id = %signal.market_id,
            direction = ?signal.direction,
            ev        = signal.expected_value,
            confidence = signal.confidence,
            "graph_arb_agent: signal emitted"
        );
        if let Err(e) = self.bus.publish(Event::Signal(signal)) {
            warn!("graph_arb_agent: failed to publish signal: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Pure signal generation — no I/O or async
// ---------------------------------------------------------------------------

/// Attempt to generate a trade signal from the current state for one market.
///
/// Returns `Some(TradeSignal)` when all gates pass; `None` otherwise.
fn try_signal(
    market_id: &str,
    entry: &MarketPriceEntry,
    config: &GraphArbConfig,
) -> Option<TradeSignal> {
    let implied_prob = entry.implied_prob?;
    let market_prob = entry.market_prob?;

    // Gate 1: Price bounds — avoid near-certain markets where liquidity is poor.
    if market_prob < config.min_market_price || market_prob > config.max_market_price {
        return None;
    }

    // Gate 2: Edge size.
    let delta = implied_prob - market_prob;
    if delta.abs() < config.min_edge_threshold {
        return None;
    }

    let direction = if delta > 0.0 {
        TradeDirection::Buy
    } else {
        TradeDirection::Sell
    };

    // Gate 3: Expected value after trading costs.
    let ev = delta.abs() - config.trading_cost;
    if ev <= 0.0 {
        return None;
    }

    // Confidence proportional to edge size, capped at 1.
    let confidence = (delta.abs() / config.confidence_scale).clamp(0.0, 1.0);

    // Kelly position sizing.
    let raw_kelly = match direction {
        TradeDirection::Buy => {
            let denom = 1.0 - market_prob;
            if denom < 1e-12 {
                return None;
            }
            delta / denom
        }
        TradeDirection::Sell => {
            if market_prob < 1e-12 {
                return None;
            }
            (-delta) / market_prob
        }
        TradeDirection::Arbitrage => return None,
    };
    let position_fraction = (raw_kelly.max(0.0) * config.kelly_fraction)
        .min(config.max_position_fraction);

    Some(TradeSignal {
        market_id: market_id.to_string(),
        direction,
        expected_value: ev,
        position_fraction,
        // Graph-implied probability plays the role of "model probability".
        posterior_prob: implied_prob,
        market_prob,
        confidence,
        timestamp: Utc::now(),
        source: "graph_arb_agent".to_string(),
    })
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> GraphArbConfig {
        GraphArbConfig::default()
    }

    fn entry(implied: Option<f64>, market: Option<f64>) -> MarketPriceEntry {
        MarketPriceEntry {
            implied_prob: implied,
            market_prob: market,
        }
    }

    #[test]
    fn no_signal_without_implied_prob() {
        let e = entry(None, Some(0.5));
        assert!(try_signal("M", &e, &config()).is_none());
    }

    #[test]
    fn no_signal_without_market_prob() {
        let e = entry(Some(0.7), None);
        assert!(try_signal("M", &e, &config()).is_none());
    }

    #[test]
    fn buy_signal_on_positive_delta() {
        // implied=0.70, market=0.50 → delta=0.20 > threshold(0.04)
        let e = entry(Some(0.70), Some(0.50));
        let s = try_signal("M", &e, &config()).unwrap();
        assert_eq!(s.direction, TradeDirection::Buy);
        assert!((s.expected_value - (0.20 - 0.003)).abs() < 1e-9);
        assert!(s.confidence > 0.0 && s.confidence <= 1.0);
        assert!(s.position_fraction > 0.0 && s.position_fraction <= 0.05);
    }

    #[test]
    fn sell_signal_on_negative_delta() {
        // implied=0.30, market=0.50 → delta=-0.20
        let e = entry(Some(0.30), Some(0.50));
        let s = try_signal("M", &e, &config()).unwrap();
        assert_eq!(s.direction, TradeDirection::Sell);
        assert!(s.expected_value > 0.0);
    }

    #[test]
    fn no_signal_below_threshold() {
        // delta=0.02 < min_edge_threshold=0.04
        let e = entry(Some(0.52), Some(0.50));
        assert!(try_signal("M", &e, &config()).is_none());
    }

    #[test]
    fn no_signal_when_ev_negative_after_cost() {
        // delta = 0.005, trading_cost = 0.003 → EV = 0.002 > 0 but barely
        // Actually 0.005 - 0.003 = 0.002 > 0, but threshold is 0.04 so this won't pass gate 2.
        // Let's use a threshold-straddling case: increase min_edge_threshold to match.
        let cfg = GraphArbConfig {
            min_edge_threshold: 0.002,
            trading_cost: 0.003,
            ..GraphArbConfig::default()
        };
        // delta = 0.002, ev = 0.002 - 0.003 = -0.001 → gate 3 fails
        let e = entry(Some(0.502), Some(0.500));
        assert!(try_signal("M", &e, &cfg).is_none());
    }

    #[test]
    fn no_signal_outside_price_bounds() {
        // market at 0.98 > max_market_price=0.95
        let e = entry(Some(0.70), Some(0.98));
        assert!(try_signal("M", &e, &config()).is_none());
    }

    #[test]
    fn confidence_capped_at_one() {
        // Enormous delta (0.60 >> confidence_scale=0.20) → confidence = 1.0
        let e = entry(Some(0.90), Some(0.30));
        let s = try_signal("M", &e, &config()).unwrap();
        assert!((s.confidence - 1.0).abs() < 1e-9);
    }
}
