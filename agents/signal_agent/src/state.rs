// agents/signal_agent/src/state.rs
// Shared mutable state for the Signal Agent, protected by an async RwLock.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Per-market belief snapshot
// ---------------------------------------------------------------------------

/// Posterior belief stored from the most recent `PosteriorUpdate` event.
#[derive(Debug, Clone)]
pub struct MarketBelief {
    /// Bayesian posterior probability.
    pub posterior_prob: f64,
    /// Evidence-density confidence in [0, 1].
    pub confidence: f64,
}

// ---------------------------------------------------------------------------
// Agent state
// ---------------------------------------------------------------------------

/// All mutable state owned by one `SignalAgent` instance.
///
/// Guarded by `Arc<RwLock<>>` so the agent can share it with health-check
/// or monitoring tasks without cloning the full map.
#[derive(Debug, Default)]
pub struct SignalAgentState {
    /// Latest market-reported prices keyed by `market_id`.
    /// Populated on every `Event::Market` received.
    pub latest_market_prices: HashMap<String, f64>,

    /// Latest Bayesian beliefs keyed by `market_id`.
    /// Populated on every `Event::Posterior` received.
    pub latest_beliefs: HashMap<String, MarketBelief>,

    /// Running count of `TradeSignal` events emitted since startup.
    pub signals_emitted: u64,
}

pub type SharedSignalState = Arc<RwLock<SignalAgentState>>;

/// Construct a new, empty shared-state handle.
pub fn new_shared_state() -> SharedSignalState {
    Arc::new(RwLock::new(SignalAgentState::default()))
}
