// crates/simulation_engine/src/types.rs
//
// Core data structures for the Simulation Engine.

use serde::{Deserialize, Serialize};

use common::MarketUpdate;

// ---------------------------------------------------------------------------
// Simulation mode
// ---------------------------------------------------------------------------

/// Determines what the `SimulationEngine` does when `run()` is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SimulationMode {
    /// Passively subscribe to the live Event Bus and collect performance
    /// metrics from `Event::Execution` and `Event::Portfolio` events.
    /// This is the default mode and is safe to run alongside the live scanner.
    #[default]
    LiveObserver,

    /// Run an internal Monte Carlo simulation (does **not** publish to the
    /// Event Bus) and log aggregated results before transitioning to
    /// `LiveObserver` mode.
    MonteCarlo,

    /// Load historical NDJSON files from `config.history_dir`, publish them
    /// as `Event::Market` updates so the full pipeline runs on real data, then
    /// transition to `LiveObserver` mode.
    HistoricalReplay,

    /// Run a two-point volatility parameter sweep (low / high) using the
    /// internal Monte Carlo engine, log comparison results, then transition
    /// to `LiveObserver` mode.
    ParameterSweep,
}

// ---------------------------------------------------------------------------
// SimulationResult
// ---------------------------------------------------------------------------

/// Performance summary for a single simulation run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationResult {
    /// Zero-based index of this run within its batch.
    pub run_id: usize,
    /// `(final_equity − initial_capital) / initial_capital`.
    pub total_return: f64,
    /// Annualised Sharpe ratio (mean return / σ × √252).
    pub sharpe_ratio: f64,
    /// Maximum peak-to-trough drawdown fraction in `[0, 1]`.
    pub max_drawdown: f64,
    /// Fraction of closed trades with positive PnL.
    pub win_rate: f64,
    /// Total number of round-trip trades closed during this run.
    pub trades_executed: usize,
    /// Portfolio equity at the end of the run (USD).
    pub final_equity: f64,
}

impl Default for SimulationResult {
    fn default() -> Self {
        Self {
            run_id:          0,
            total_return:    0.0,
            sharpe_ratio:    0.0,
            max_drawdown:    0.0,
            win_rate:        0.0,
            trades_executed: 0,
            final_equity:    0.0,
        }
    }
}

// ---------------------------------------------------------------------------
// SimulationTick
// ---------------------------------------------------------------------------

/// A single time-step in a simulation: one or more market probability updates.
#[derive(Debug, Clone)]
pub struct SimulationTick {
    /// Millisecond-precision UNIX timestamp of this tick.
    pub timestamp: i64,
    /// Market updates to publish (or process internally) for this tick.
    pub market_updates: Vec<MarketUpdate>,
}

// ---------------------------------------------------------------------------
// SweepResult
// ---------------------------------------------------------------------------

/// Aggregated results for one configuration point in a parameter sweep.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweepResult {
    /// Human-readable label for this configuration point.
    pub label: String,
    /// Volatility value used for this sweep point.
    pub mc_config_volatility: f64,
    /// Drift value used for this sweep point.
    pub mc_config_drift: f64,
    /// Individual results for every Monte Carlo run at this configuration.
    pub results: Vec<SimulationResult>,
    /// Mean `total_return` across all runs.
    pub avg_return: f64,
    /// Mean `sharpe_ratio` across all runs.
    pub avg_sharpe: f64,
    /// Mean `max_drawdown` across all runs.
    pub avg_max_drawdown: f64,
}

impl SweepResult {
    pub fn from_results(
        label: String,
        volatility: f64,
        drift: f64,
        results: Vec<SimulationResult>,
    ) -> Self {
        let n = results.len() as f64;
        let avg_return = if n > 0.0 {
            results.iter().map(|r| r.total_return).sum::<f64>() / n
        } else {
            0.0
        };
        let avg_sharpe = if n > 0.0 {
            results.iter().map(|r| r.sharpe_ratio).sum::<f64>() / n
        } else {
            0.0
        };
        let avg_max_drawdown = if n > 0.0 {
            results.iter().map(|r| r.max_drawdown).sum::<f64>() / n
        } else {
            0.0
        };
        Self {
            label,
            mc_config_volatility: volatility,
            mc_config_drift: drift,
            results,
            avg_return,
            avg_sharpe,
            avg_max_drawdown,
        }
    }
}
