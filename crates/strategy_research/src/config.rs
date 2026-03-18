// crates/strategy_research/src/config.rs

use serde::{Deserialize, Serialize};

/// Configuration for the `StrategyResearchEngine`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchConfig {
    /// Hypotheses to generate per research cycle.
    pub batch_size: usize,
    /// Seconds between research cycles.
    pub research_interval_secs: u64,
    /// Synthetic market ticks to simulate per backtest run.
    pub backtest_ticks: usize,
    /// Number of synthetic markets per backtest.
    pub backtest_markets: usize,
    /// Ticks a position is held before forced close.
    pub hold_ticks: usize,
    /// Minimum annualised Sharpe ratio to promote a strategy.
    pub min_sharpe: f64,
    /// Maximum drawdown fraction allowed for promotion (e.g. 0.10 = 10 %).
    pub max_drawdown: f64,
    /// Minimum trade count for a strategy to be considered valid.
    pub min_trades: usize,
    /// Maximum number of strategies kept in the registry (capacity-eviction by Sharpe).
    pub registry_capacity: usize,
    /// RNG seed for reproducible research cycles (`None` = random per cycle).
    pub seed: Option<u64>,
    /// Per-tick price volatility for synthetic market paths.
    pub backtest_volatility: f64,
    /// Per-tick price drift for synthetic market paths.
    pub backtest_drift: f64,
    /// Correlation between synthetic markets (common-factor model).
    pub backtest_correlation: f64,
    /// Simulated starting capital for PnL tracking.
    pub initial_capital: f64,
    /// Maximum concurrent backtest tasks (semaphore bound).
    pub max_parallel_backtests: usize,
}

impl Default for ResearchConfig {
    fn default() -> Self {
        Self {
            batch_size: 200,
            research_interval_secs: 300,
            backtest_ticks: 1_000,
            backtest_markets: 4,
            hold_ticks: 20,
            min_sharpe: 1.0,
            max_drawdown: 0.10,
            min_trades: 100,
            registry_capacity: 50,
            seed: None,
            backtest_volatility: 0.015,
            backtest_drift: 0.0,
            backtest_correlation: 0.3,
            initial_capital: 10_000.0,
            max_parallel_backtests: 8,
        }
    }
}

impl ResearchConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.batch_size == 0 {
            return Err("batch_size must be > 0".into());
        }
        if self.research_interval_secs == 0 {
            return Err("research_interval_secs must be > 0".into());
        }
        if self.backtest_ticks < 100 {
            return Err("backtest_ticks must be >= 100".into());
        }
        if self.backtest_markets < 2 {
            return Err("backtest_markets must be >= 2".into());
        }
        if self.hold_ticks == 0 {
            return Err("hold_ticks must be > 0".into());
        }
        if !(0.0..=1.0).contains(&self.max_drawdown) {
            return Err("max_drawdown must be in [0, 1]".into());
        }
        if self.registry_capacity == 0 {
            return Err("registry_capacity must be > 0".into());
        }
        if self.max_parallel_backtests == 0 {
            return Err("max_parallel_backtests must be > 0".into());
        }
        if !(0.0..=1.0).contains(&self.backtest_correlation) {
            return Err("backtest_correlation must be in [0, 1]".into());
        }
        if self.backtest_volatility <= 0.0 {
            return Err("backtest_volatility must be > 0".into());
        }
        if self.initial_capital <= 0.0 {
            return Err("initial_capital must be > 0".into());
        }
        Ok(())
    }
}
