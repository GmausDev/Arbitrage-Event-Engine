// crates/execution_sim/src/state.rs

use std::sync::Arc;

use common::Portfolio;
use tokio::sync::RwLock;

/// Mutable runtime state for the `ExecutionSimulator`.
#[derive(Debug)]
pub struct ExecutionState {
    /// Current simulated portfolio (positions, exposure, PnL).
    pub portfolio:        Portfolio,
    /// Highest equity observed since start — used to track unrealised drawdown.
    pub peak_equity:      f64,
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

impl Default for ExecutionState {
    fn default() -> Self {
        Self {
            portfolio:        Portfolio::default(),
            // `bankroll` and `peak_equity` are sentinel 0.0 here; they must be
            // initialised from `ExecutionConfig::bankroll` before first use.
            // `ExecutionSimulator::new` always sets them correctly.
            peak_equity:      0.0,
            bankroll:         0.0,
            orders_processed: 0,
            orders_filled:    0,
            orders_partial:   0,
            total_slippage:   0.0,
        }
    }
}

pub type SharedExecutionState = Arc<RwLock<ExecutionState>>;

pub fn new_shared_state() -> SharedExecutionState {
    Arc::new(RwLock::new(ExecutionState::default()))
}
