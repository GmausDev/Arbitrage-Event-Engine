// agents/graph_arb_agent/src/state.rs

use std::{collections::HashMap, sync::Arc};
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
    /// Per-market cached probabilities.
    pub markets: HashMap<String, MarketPriceEntry>,
    /// Total events processed (for test synchronisation).
    pub events_processed: u64,
    /// Total signals emitted.
    pub signals_emitted: u64,
}

pub type SharedGraphArbState = Arc<RwLock<GraphArbState>>;

pub fn new_shared_state() -> SharedGraphArbState {
    Arc::new(RwLock::new(GraphArbState::default()))
}
