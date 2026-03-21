// crates/execution_sim/src/state.rs

use std::sync::Arc;

use tokio::sync::RwLock;

/// Mutable runtime state for the `ExecutionSimulator`.
#[derive(Debug, Default)]
pub struct ExecutionState {
    /// Bankroll snapshot (kept in sync with `ExecutionConfig::bankroll`).
    pub bankroll:         f64,
    // ── Counters (reset to zero at construction) ───────────────────────────
    pub orders_processed: u64,
    pub orders_filled:    u64,
    /// Orders that received a partial fill (executed_quantity < requested).
    pub orders_partial:   u64,
    /// Sum of absolute slippage fractions across all filled orders.
    pub total_slippage:   f64,
}

pub type SharedExecutionState = Arc<RwLock<ExecutionState>>;

pub fn new_shared_state() -> SharedExecutionState {
    Arc::new(RwLock::new(ExecutionState::default()))
}
