// agents/graph_arb_agent/src/state.rs

use std::{collections::HashMap, sync::Arc};
use chrono::{DateTime, Utc};
use common::events::MarketSnapshot;
use tokio::sync::RwLock;

/// Per-market state maintained by the Graph Arbitrage Agent.
#[derive(Debug, Clone, Default)]
pub struct MarketPriceEntry {
    /// Latest graph-implied probability from BFS propagation (GraphUpdate).
    pub implied_prob: Option<f64>,
    /// Latest observed market price from a MarketUpdate.
    pub market_prob: Option<f64>,
}

/// Shared mutable state for the Graph Arbitrage Agent.
#[derive(Debug, Default)]
pub struct GraphArbState {
    /// Per-market cached probabilities (used by graph-arb strategy).
    pub markets: HashMap<String, MarketPriceEntry>,
    /// Full snapshots from all platforms (used by cross-platform strategy).
    /// Keyed by `market_id`.
    pub snapshots: HashMap<String, MarketSnapshot>,
    /// Last time a cross-platform arb signal was emitted for each pair.
    /// Keys are canonical `(cheap_id, dear_id)` ordered lexicographically.
    pub xplatform_last_emitted: HashMap<(String, String), DateTime<Utc>>,
    /// Total events processed (for test synchronisation).
    pub events_processed: u64,
    /// Total signals emitted (graph-arb + cross-platform combined).
    pub signals_emitted: u64,
    /// Cross-platform signals emitted (subset of signals_emitted).
    pub xplatform_signals_emitted: u64,
}

pub type SharedGraphArbState = Arc<RwLock<GraphArbState>>;

pub fn new_shared_state() -> SharedGraphArbState {
    Arc::new(RwLock::new(GraphArbState::default()))
}
