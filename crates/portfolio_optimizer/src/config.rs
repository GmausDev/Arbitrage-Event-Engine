// crates/portfolio_optimizer/src/config.rs

use serde::{Deserialize, Serialize};

/// Configuration for the Portfolio Optimizer's allocation algorithm.
///
/// All fraction fields are in `[0.0, 1.0]` and represent a proportion of the
/// optimizer's declared `total_capital`.  They do **not** map directly to USD.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AllocationConfig {
    /// Maximum fraction of available capital that may be deployed in any single
    /// market.  Signals requesting more will be capped here.
    ///
    /// Default: `0.10` (10 %).
    pub max_allocation_per_market: f64,

    /// Maximum aggregate fraction that may be deployed across all markets within
    /// the same niche (cluster prefix, e.g. `"US"` for `"US_ELECTION_*"`).
    ///
    /// Default: `0.25` (25 %).
    pub max_allocation_per_niche: f64,

    /// Sum of all allocations must not exceed `total_capital`.  Set to `1.0`
    /// to allow full deployment of available capital.
    ///
    /// Default: `1.0` (100 % of capital_available).
    pub total_capital: f64,

    /// Scale factor for the intra-cluster correlation penalty.  When `0.0` the
    /// penalty is disabled entirely; when `1.0` a fully saturated niche brings
    /// a later signal's allocation to zero.
    ///
    /// Default: `0.5`.
    pub correlation_penalty_factor: f64,

    /// Width of the signal-accumulation window in milliseconds.  The optimizer
    /// collects `Event::Signal` messages for this duration, then runs the
    /// batch-allocation algorithm and emits `Event::OptimizedSignal` for each
    /// non-zero allocation before clearing the batch.
    ///
    /// Default: `50` ms.
    pub tick_ms: u64,

    /// Assumed intra-cluster correlation when no explicit edge weight is
    /// available.  Used to scale the penalty for markets sharing a cluster
    /// prefix but having no recorded graph relationship.
    ///
    /// Default: `0.4`.
    pub default_cluster_correlation: f64,

    /// Reference bankroll in USD used to convert the portfolio's absolute
    /// `exposure` field into the fractional `capital_available` the allocator
    /// works with.  **Must match `RiskConfig::bankroll` (if that field exists)
    /// or the default `RiskState::bankroll` of `10 000.0`.**
    ///
    /// Default: `10_000.0`.
    pub bankroll: f64,

    /// When `true`, the optimizer ignores individual `Event::Signal` events and
    /// instead processes `Event::TopSignalsBatch` emitted by `SignalPriorityEngine`.
    ///
    /// This activates the fast/slow signal split: fast signals bypass the optimizer
    /// via `Event::FastSignal` → `RiskEngine`, while slow signals arrive as a
    /// pre-ranked batch via `Event::TopSignalsBatch` → this optimizer.
    ///
    /// Default: `true`.
    pub use_priority_engine: bool,
}

impl Default for AllocationConfig {
    fn default() -> Self {
        Self {
            max_allocation_per_market:   0.10,
            max_allocation_per_niche:    0.25,
            total_capital:               1.0,
            correlation_penalty_factor:  0.5,
            tick_ms:                     50,
            default_cluster_correlation: 0.4,
            bankroll:                    10_000.0,
            use_priority_engine:         true,
        }
    }
}
