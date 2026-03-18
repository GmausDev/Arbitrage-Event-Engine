// crates/execution_sim/src/config.rs

use serde::{Deserialize, Serialize};

/// Configuration for the Execution Simulator.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExecutionConfig {
    /// When `true`, apply a Gaussian slippage perturbation to the fill price.
    ///
    /// Set to `false` for deterministic backtesting.
    pub simulate_slippage: bool,

    /// Standard deviation of the fill-price perturbation as a fraction of the
    /// mid-price.  E.g. `0.002` → ±0.2% typical slippage.
    pub slippage_std_dev: f64,

    /// Probability `[0, 1]` that any given order receives a **partial** fill.
    ///
    /// When a partial fill occurs, the executed quantity is drawn uniformly
    /// from `[0.5 × requested, requested]`.
    pub partial_fill_probability: f64,

    /// Reference bankroll in USD.
    ///
    /// Used to convert fractional allocations to absolute dollar sizes when
    /// recording portfolio exposure.  **Must match `RiskConfig::bankroll`
    /// (the default `RiskState::bankroll` is `10_000.0`).**
    pub bankroll: f64,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            simulate_slippage:        true,
            slippage_std_dev:         0.002,  // 0.2 %
            partial_fill_probability: 0.10,   // 10 % chance of partial fill
            bankroll:                 10_000.0,
        }
    }
}
