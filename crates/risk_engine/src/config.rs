// crates/risk_engine/src/config.rs

use serde::{Deserialize, Serialize};

/// Configuration thresholds for the Risk Engine.
///
/// All exposure fields are expressed as fractions of bankroll in `[0.0, 1.0]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RiskConfig {
    /// Reference bankroll in USD. Must match portfolio_engine and execution_sim.
    pub bankroll: f64,

    /// Maximum position size per market as a fraction of bankroll.
    ///
    /// Signals with a larger `position_fraction` are resized down to this cap.
    pub max_position_fraction: f64,

    /// Maximum total capital deployed across all open positions.
    ///
    /// Signals that would push total exposure beyond this are resized or rejected.
    pub max_total_exposure: f64,

    /// Maximum capital deployed within a single market cluster.
    ///
    /// Cluster is derived from the market ID prefix: `"US_ELECTION_TRUMP"` →
    /// cluster `"US"`.
    pub max_cluster_exposure: f64,

    /// Minimum expected value required to approve a signal.
    ///
    /// Signals with `expected_value < min_expected_value` are rejected immediately.
    pub min_expected_value: f64,

    /// Maximum drawdown from peak equity before trading is suspended.
    ///
    /// `drawdown = (peak_equity − current_equity) / peak_equity`
    pub max_drawdown: f64,

    /// When `true` (the default), the engine evaluates raw `Event::Signal` events
    /// directly — use this when no `PortfolioOptimizer` is in the pipeline.
    ///
    /// Set to `false` when `PortfolioOptimizer` is active.  In that mode the
    /// engine ignores raw signals and only evaluates `Event::OptimizedSignal`,
    /// preventing every intent from being approved twice.
    pub accept_raw_signals: bool,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            bankroll:              10_000.0,
            max_position_fraction: 0.05,
            max_total_exposure:    0.65,
            max_cluster_exposure:  0.25,
            min_expected_value:    0.02,
            max_drawdown:          0.20,
            // false: SignalPriorityEngine is the default routing mode.
            // Set to true only when running without the priority engine.
            accept_raw_signals:    false,
        }
    }
}
