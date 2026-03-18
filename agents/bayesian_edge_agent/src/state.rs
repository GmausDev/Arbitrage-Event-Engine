// agents/bayesian_edge_agent/src/state.rs

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Utc};
use tokio::sync::RwLock;

/// Per-market belief state maintained by the Bayesian Edge Agent.
#[derive(Debug, Clone, Default)]
pub struct MarketBeliefEntry {
    /// Bayesian posterior probability from the most recent `PosteriorUpdate`.
    pub posterior_prob: Option<f64>,
    /// Evidence-density confidence in [0, 1] from `PosteriorUpdate`.
    pub posterior_confidence: Option<f64>,
    /// Latest observed market price from `MarketUpdate`.
    pub market_prob: Option<f64>,
    /// Latest graph-implied probability from `GraphUpdate` (optional damping).
    pub implied_prob: Option<f64>,
    /// Magnitude of the most recent information shock for this market [0, 1].
    /// Zero when no shock has been received.
    pub shock_magnitude: f64,
    /// Timestamp of the most recent shock, used for freshness checks.
    pub shock_ts: Option<DateTime<Utc>>,
}

/// Shared mutable state for the Bayesian Edge Agent.
#[derive(Debug, Default)]
pub struct BayesianEdgeState {
    /// Per-market cached beliefs and prices.
    pub beliefs: HashMap<String, MarketBeliefEntry>,
    /// Total events processed (for test synchronisation).
    pub events_processed: u64,
    /// Total signals emitted.
    pub signals_emitted: u64,
}

pub type SharedBayesianEdgeState = Arc<RwLock<BayesianEdgeState>>;

pub fn new_shared_state() -> SharedBayesianEdgeState {
    Arc::new(RwLock::new(BayesianEdgeState::default()))
}
