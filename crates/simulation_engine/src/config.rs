// crates/simulation_engine/src/config.rs
//
// Configuration structs for the Simulation Engine.

use serde::{Deserialize, Serialize};

use crate::types::SimulationMode;

// ---------------------------------------------------------------------------
// SimulationConfig
// ---------------------------------------------------------------------------

/// Top-level runtime configuration for `SimulationEngine`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SimulationConfig {
    /// Operating mode — controls what `run()` does on startup.
    pub mode: SimulationMode,

    /// Milliseconds to wait between publishing ticks in `HistoricalReplay`
    /// mode.  Set to `0` for as-fast-as-possible replay (useful in tests).
    pub tick_interval_ms: u64,

    /// Starting bankroll in USD.  Used as the baseline for return computation
    /// in the internal Monte Carlo.
    pub initial_capital: f64,

    /// Maximum number of ticks per Monte Carlo run.
    pub max_ticks: usize,

    /// Path to the directory containing NDJSON historical market files.
    /// Relative paths are resolved from the process working directory.
    pub history_dir: String,

    /// Configuration for the Monte Carlo simulation paths.
    pub monte_carlo: MonteCarloConfig,

    /// Synthetic markets used by the generator and internal MC simulation.
    /// Defaults to three uncorrelated markets at 0.45 / 0.50 / 0.55.
    pub markets: Vec<MarketSpec>,

    /// Minimum absolute probability deviation from `initial_prob` required to
    /// open a simulated trade in the internal MC model.
    pub signal_threshold: f64,

    /// Fraction of current equity to allocate per simulated trade.
    pub position_fraction: f64,
}

impl Default for SimulationConfig {
    fn default() -> Self {
        Self {
            mode:              SimulationMode::LiveObserver,
            tick_interval_ms:  1_000,
            initial_capital:   10_000.0,
            max_ticks:         1_000,
            history_dir:       "data/historical_markets".to_string(),
            monte_carlo:       MonteCarloConfig::default(),
            markets: vec![
                MarketSpec { id: "SIM-MARKET-A".to_string(), initial_prob: 0.50, liquidity: 10_000.0 },
                MarketSpec { id: "SIM-MARKET-B".to_string(), initial_prob: 0.45, liquidity:  8_000.0 },
                MarketSpec { id: "SIM-MARKET-C".to_string(), initial_prob: 0.55, liquidity:  6_000.0 },
            ],
            signal_threshold:  0.05,
            position_fraction: 0.05,
        }
    }
}

impl SimulationConfig {
    /// Validate all configuration fields.  Returns `Err` with a description
    /// of the first invalid field encountered.
    pub fn validate(&self) -> Result<(), String> {
        if self.tick_interval_ms == 0 && self.mode == SimulationMode::HistoricalReplay {
            // tick_interval_ms == 0 is allowed for fast replay in tests; no error.
        }
        if !self.initial_capital.is_finite() || self.initial_capital <= 0.0 {
            return Err(format!(
                "initial_capital must be finite and positive, got {}",
                self.initial_capital
            ));
        }
        if self.max_ticks == 0 {
            return Err("max_ticks must be > 0".to_string());
        }
        if !self.signal_threshold.is_finite() || self.signal_threshold <= 0.0 {
            return Err(format!(
                "signal_threshold must be finite and positive, got {}",
                self.signal_threshold
            ));
        }
        if !self.position_fraction.is_finite()
            || self.position_fraction <= 0.0
            || self.position_fraction > 1.0
        {
            return Err(format!(
                "position_fraction must be in (0, 1], got {}",
                self.position_fraction
            ));
        }
        self.monte_carlo.validate()?;

        // Validate each market spec.
        for (i, spec) in self.markets.iter().enumerate() {
            if spec.id.is_empty() {
                return Err(format!("markets[{i}].id must not be empty"));
            }
            if !spec.initial_prob.is_finite()
                || spec.initial_prob < 0.01
                || spec.initial_prob > 0.99
            {
                return Err(format!(
                    "markets[{i}].initial_prob must be in [0.01, 0.99], got {}",
                    spec.initial_prob
                ));
            }
            if !spec.liquidity.is_finite() || spec.liquidity < 0.0 {
                return Err(format!(
                    "markets[{i}].liquidity must be finite and >= 0, got {}",
                    spec.liquidity
                ));
            }
        }

        // Empty markets list makes MC/sweep produce meaningless zero results.
        if self.markets.is_empty()
            && matches!(
                self.mode,
                SimulationMode::MonteCarlo | SimulationMode::ParameterSweep
            )
        {
            return Err(
                "markets must not be empty for MonteCarlo/ParameterSweep mode".to_string(),
            );
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// MonteCarloConfig
// ---------------------------------------------------------------------------

/// Parameters for synthetic market path generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MonteCarloConfig {
    /// Number of independent simulation runs.
    pub runs: usize,

    /// Per-tick standard deviation of the probability random walk.
    /// Higher values → more volatile market paths.
    pub volatility: f64,

    /// Per-tick mean drift applied to every market's probability.
    /// Positive drift → prices trend upward over time.
    pub drift: f64,

    /// Optional PRNG seed for reproducible runs.  `None` → seeded from
    /// entropy (non-deterministic).
    pub seed: Option<u64>,

    /// Pairwise Pearson correlation between all synthetic markets in `[-1, 1]`.
    /// `0.0` → independent; `1.0` → perfectly co-moving.
    pub correlation: f64,
}

impl Default for MonteCarloConfig {
    fn default() -> Self {
        Self {
            runs:        100,
            volatility:  0.02,
            drift:       0.0,
            seed:        None,
            correlation: 0.3,
        }
    }
}

impl MonteCarloConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.runs == 0 {
            return Err("monte_carlo.runs must be > 0".to_string());
        }
        if !self.volatility.is_finite() || self.volatility < 0.0 {
            return Err(format!(
                "monte_carlo.volatility must be finite and ≥ 0, got {}",
                self.volatility
            ));
        }
        if !self.drift.is_finite() {
            return Err("monte_carlo.drift must be finite".to_string());
        }
        if !self.correlation.is_finite()
            || self.correlation < -1.0
            || self.correlation > 1.0
        {
            return Err(format!(
                "monte_carlo.correlation must be in [-1, 1], got {}",
                self.correlation
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// MarketSpec
// ---------------------------------------------------------------------------

/// Describes a single synthetic market used by the generator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketSpec {
    /// Unique market identifier (used as `MarketNode::id`).
    pub id: String,
    /// Starting probability in `[0.01, 0.99]`.
    pub initial_prob: f64,
    /// Simulated liquidity in USD (informational, does not affect path).
    pub liquidity: f64,
}

// ---------------------------------------------------------------------------
// ParameterSweepPoint
// ---------------------------------------------------------------------------

/// A single point in a parameter sweep: a label and the MC config to use.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterSweepPoint {
    /// Short label displayed in logs and sweep results.
    pub label: String,
    /// Monte Carlo configuration to apply for this sweep point.
    pub mc_config: MonteCarloConfig,
}
