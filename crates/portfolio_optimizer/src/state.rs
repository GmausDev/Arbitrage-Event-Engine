// crates/portfolio_optimizer/src/state.rs

use std::collections::HashMap;
use std::sync::Arc;

use common::TradeSignal;
use tokio::sync::RwLock;

/// Runtime state maintained by the Portfolio Optimizer.
#[derive(Debug)]
pub struct OptimizerState {
    /// Current net position per market (fraction of capital).
    ///
    /// Updated when `Event::Portfolio` arrives from execution_sim.
    pub positions: HashMap<String, f64>,

    /// Capital fraction available for new allocations (default `1.0`).
    pub capital_available: f64,

    /// Signals buffered in the current tick window, waiting for the next
    /// flush to run the allocation algorithm.
    pub pending_signals: Vec<TradeSignal>,

    // ── diagnostics ──────────────────────────────────────────────────────────

    /// Total `Signal` events received across all ticks.
    pub signals_buffered: u64,

    /// Total `OptimizedSignal` events published across all ticks.
    pub signals_emitted: u64,

    /// Number of tick-flush cycles completed.
    pub batches_processed: u64,
}

impl Default for OptimizerState {
    fn default() -> Self {
        Self {
            positions:         HashMap::new(),
            capital_available: 1.0,
            pending_signals:   Vec::new(),
            signals_buffered:  0,
            signals_emitted:   0,
            batches_processed: 0,
        }
    }
}

pub type SharedOptimizerState = Arc<RwLock<OptimizerState>>;

/// Create a fresh shared state.
pub fn new_shared_state() -> SharedOptimizerState {
    Arc::new(RwLock::new(OptimizerState::default()))
}
