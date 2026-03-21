// crates/simulation_engine/src/monte_carlo.rs
//
// High-performance Monte Carlo simulation for portfolio risk estimation.
//
// # Design
//
// `MonteCarloSimulator` runs thousands of independent market paths in parallel
// using rayon. Each path:
//   1. Starts from a `SimulationState` snapshot (cloned; live state never mutated).
//   2. Evolves market probabilities per tick via: Gaussian noise + mean reversion + drift.
//   3. Optionally injects random shocks (configurable probability + magnitude).
//   4. Updates simplified Bayesian posteriors (weighted blend with market price).
//   5. Runs a mean-reversion signal strategy: open/close positions based on edge.
//   6. Tracks portfolio equity, drawdown, and per-tick returns.
//
// # Parallelism
//
// `rayon::par_iter` divides paths across the CPU thread pool. Each path owns its
// own `StdRng` (seeded from `config.seed + path_id` for full reproducibility) so
// there is zero shared mutable state.
//
// # Offline operation
//
// All computation is self-contained. No Event Bus messages are published or
// consumed. This module consumes only `SimulationState` snapshots.

use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use rand_distr::StandardNormal;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::config::MarketSpec;

// ---------------------------------------------------------------------------
// SimulationState — the input snapshot
// ---------------------------------------------------------------------------

/// Offline snapshot of the market system used as the starting point for
/// every Monte Carlo path.
///
/// Build from live data via `SimulationState::from_market_specs` or construct
/// directly with `markets` and `portfolio_value`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationState {
    /// Per-market probability state at the start of the simulation.
    pub markets: Vec<MarketStateSnapshot>,
    /// Current total portfolio value in USD (bankroll + unrealised PnL).
    pub portfolio_value: f64,
}

impl SimulationState {
    /// Convenience constructor: build a `SimulationState` from a list of
    /// `MarketSpec` values (as used by the existing MC generator) with
    /// all three probability fields initialised to `initial_prob`.
    pub fn from_market_specs(markets: &[MarketSpec], portfolio_value: f64) -> Self {
        let snapshots = markets
            .iter()
            .map(|m| MarketStateSnapshot {
                id:            m.id.clone(),
                probability:   m.initial_prob,
                implied_prob:  m.initial_prob,
                posterior_prob: m.initial_prob,
                liquidity:     m.liquidity,
            })
            .collect();
        Self { markets: snapshots, portfolio_value }
    }
}

/// State of a single market within the simulation snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketStateSnapshot {
    /// Unique market identifier.
    pub id: String,
    /// Current market-implied (exchange) probability ∈ [0, 1].
    pub probability: f64,
    /// Graph-propagated implied probability ∈ [0, 1].
    /// Used as the mean-reversion target during simulation.
    pub implied_prob: f64,
    /// Bayesian posterior probability ∈ [0, 1].
    /// Used to compute the signal edge: `posterior − market`.
    pub posterior_prob: f64,
    /// Available liquidity in USD (informational; does not affect path).
    pub liquidity: f64,
}

// ---------------------------------------------------------------------------
// MonteCarloResult — aggregated output
// ---------------------------------------------------------------------------

/// Aggregated risk metrics across all simulation paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonteCarloResult {
    /// Mean final-portfolio return: `(final_value − initial) / initial`,
    /// averaged across all paths.
    pub expected_return: f64,
    /// Standard deviation of final returns across paths.
    pub return_std: f64,
    /// Annualised Sharpe ratio: `expected_return / return_std × √252`.
    pub sharpe_ratio: f64,
    /// Maximum peak-to-trough drawdown fraction observed across **all** paths.
    pub max_drawdown: f64,
    /// Value-at-Risk at 95% confidence: the loss not exceeded in 95% of paths,
    /// expressed as a positive fraction of initial portfolio value.
    pub var_95: f64,
    /// Conditional VaR (Expected Shortfall) at 95%: mean loss in the worst 5%
    /// of paths, expressed as a positive fraction of initial portfolio value.
    pub cvar_95: f64,
    /// Fraction of paths that ended with a positive return.
    pub profit_probability: f64,
    /// Per-path detailed results (one entry per simulation path).
    pub paths: Vec<SimulationPathResult>,
}

impl Default for MonteCarloResult {
    fn default() -> Self {
        Self {
            expected_return:    0.0,
            return_std:         0.0,
            sharpe_ratio:       0.0,
            max_drawdown:       0.0,
            var_95:             0.0,
            cvar_95:            0.0,
            profit_probability: 0.0,
            paths:              Vec::new(),
        }
    }
}

/// Performance metrics for a single simulation path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationPathResult {
    /// Portfolio value at the end of the path.
    pub final_value: f64,
    /// Per-tick fractional returns: `(equity[t] − equity[t-1]) / equity[t-1]`.
    pub returns: Vec<f64>,
    /// Maximum peak-to-trough drawdown fraction for this path ∈ [0, 1].
    pub drawdown: f64,
}

// ---------------------------------------------------------------------------
// MonteCarloSimConfig — simulation parameters
// ---------------------------------------------------------------------------

/// Configuration for `MonteCarloSimulator`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MonteCarloSimConfig {
    /// Per-tick probability volatility σ (std dev of Gaussian noise applied
    /// to each market's probability).
    pub volatility: f64,

    /// Per-tick deterministic drift added to every market's probability.
    /// Positive → prices trend upward; negative → downward.
    pub drift: f64,

    /// Mean-reversion speed k ∈ [0, 1]:
    /// `delta += k × (implied_prob − current_prob)`.
    /// Higher values pull market prices toward the graph-implied probability faster.
    pub mean_reversion_speed: f64,

    /// Probability per tick that a shock event is injected.
    /// 0 → no shocks; 1 → shock every tick.
    pub shock_probability: f64,

    /// Maximum shock magnitude in probability units.
    /// Each shocked market shifts by a uniform random amount in [0, shock_magnitude].
    pub shock_magnitude: f64,

    /// Minimum `|posterior − market|` required to open a simulated position.
    pub signal_threshold: f64,

    /// Fraction of current equity allocated per trade.
    pub position_fraction: f64,

    /// Optional RNG seed.  `Some(s)` → path `i` uses seed `s + i` (reproducible).
    /// `None` → seeded from OS entropy (non-deterministic).
    pub seed: Option<u64>,
}

impl Default for MonteCarloSimConfig {
    fn default() -> Self {
        Self {
            volatility:           0.02,
            drift:                0.0,
            mean_reversion_speed: 0.05,
            shock_probability:    0.02,
            shock_magnitude:      0.05,
            signal_threshold:     0.05,
            position_fraction:    0.05,
            seed:                 None,
        }
    }
}

impl MonteCarloSimConfig {
    /// Validate all fields.  Returns `Err` on the first invalid value.
    pub fn validate(&self) -> Result<(), String> {
        if !self.volatility.is_finite() || self.volatility < 0.0 {
            return Err(format!("volatility must be ≥ 0, got {}", self.volatility));
        }
        if !self.drift.is_finite() {
            return Err("drift must be finite".to_string());
        }
        if !self.mean_reversion_speed.is_finite()
            || self.mean_reversion_speed < 0.0
            || self.mean_reversion_speed > 1.0
        {
            return Err(format!(
                "mean_reversion_speed must be in [0, 1], got {}",
                self.mean_reversion_speed
            ));
        }
        if !self.shock_probability.is_finite()
            || self.shock_probability < 0.0
            || self.shock_probability > 1.0
        {
            return Err(format!(
                "shock_probability must be in [0, 1], got {}",
                self.shock_probability
            ));
        }
        if !self.shock_magnitude.is_finite() || self.shock_magnitude < 0.0 {
            return Err(format!(
                "shock_magnitude must be ≥ 0, got {}",
                self.shock_magnitude
            ));
        }
        if !self.signal_threshold.is_finite() || self.signal_threshold <= 0.0 {
            return Err(format!(
                "signal_threshold must be > 0, got {}",
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
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// MonteCarloSimulator — public API
// ---------------------------------------------------------------------------

/// Runs parallelized Monte Carlo market simulations.
///
/// Construct with a validated `MonteCarloSimConfig`, then call
/// `run_monte_carlo` with an offline `SimulationState` snapshot.
///
/// # Example
/// ```ignore
/// let config = MonteCarloSimConfig::default();
/// let sim    = MonteCarloSimulator::new(config);
/// let state  = SimulationState::from_market_specs(&specs, 10_000.0);
/// let result = sim.run_monte_carlo(&state, 1_000, 100);
/// ```
pub struct MonteCarloSimulator {
    /// Simulation configuration (public for inspection).
    pub config: MonteCarloSimConfig,
}

impl MonteCarloSimulator {
    /// Create a new simulator.
    ///
    /// # Panics
    /// Panics if `config.validate()` fails.
    pub fn new(config: MonteCarloSimConfig) -> Self {
        if let Err(e) = config.validate() {
            panic!("invalid MonteCarloSimConfig: {e}");
        }
        Self { config }
    }

    /// Run `num_simulations` independent Monte Carlo paths, each of
    /// `horizon_steps` ticks, starting from `initial_state`.
    ///
    /// Paths are executed in parallel via `rayon::par_iter`.  When
    /// `config.seed` is `Some(s)`, path `i` uses seed `s.wrapping_add(i)`,
    /// giving full deterministic reproducibility.
    ///
    /// Returns `MonteCarloResult::default()` when any input is zero/empty.
    pub fn run_monte_carlo(
        &self,
        initial_state: &SimulationState,
        num_simulations: usize,
        horizon_steps: usize,
    ) -> MonteCarloResult {
        if num_simulations == 0 || horizon_steps == 0 || initial_state.markets.is_empty() {
            return MonteCarloResult::default();
        }

        info!(
            num_simulations,
            horizon_steps,
            n_markets        = initial_state.markets.len(),
            portfolio_value  = initial_state.portfolio_value,
            seed             = ?self.config.seed,
            "monte_carlo_sim: starting"
        );

        let config = &self.config;

        // Each path is fully independent — rayon divides them across threads.
        let paths: Vec<SimulationPathResult> = (0..num_simulations)
            .into_par_iter()
            .map(|sim_id| {
                let seed = config.seed.map(|s| s.wrapping_add(sim_id as u64));
                simulate_single_path(initial_state, horizon_steps, config, seed)
            })
            .collect();

        let result = aggregate_results(paths, initial_state.portfolio_value, horizon_steps);

        info!(
            expected_return  = result.expected_return,
            sharpe           = result.sharpe_ratio,
            max_drawdown     = result.max_drawdown,
            var_95           = result.var_95,
            cvar_95          = result.cvar_95,
            profit_prob      = result.profit_probability,
            "monte_carlo_sim: complete"
        );

        result
    }
}

// ---------------------------------------------------------------------------
// Core simulation: single path
// ---------------------------------------------------------------------------

/// Simulate one independent Monte Carlo market path and return its metrics.
///
/// # Market evolution model (per tick, per market i)
///
/// ```text
/// z_i   ~ N(0, 1)                                      [Gaussian noise]
/// delta = σ·z_i + k·(implied_i − p_i) + drift         [noise + reversion + drift]
/// p_i   = clamp(p_i + delta, 0.0, 1.0)
/// ```
///
/// # Shock injection
///
/// With probability `shock_probability` per tick, a random subset of markets
/// (between 1 and n_markets) is shocked by ±Uniform(0, shock_magnitude).
///
/// # Bayesian update (simplified)
///
/// `posterior_i = 0.80 × posterior_i + 0.20 × p_i`
///
/// # Strategy model
///
/// * **Entry**: open BUY when `posterior − market ≥ signal_threshold`;
///              open SELL when `market − posterior ≥ signal_threshold`.
///   Position size: `position_fraction × current_equity`.
/// * **Exit**: close when the edge reverts below `½ × signal_threshold`
///   (or reverses direction).
/// * **PnL**: BUY → `(exit_price − entry_price) × size`;
///            SELL → `(entry_price − exit_price) × size`.
fn simulate_single_path(
    initial_state: &SimulationState,
    horizon_steps: usize,
    config: &MonteCarloSimConfig,
    seed: Option<u64>,
) -> SimulationPathResult {
    let mut rng: StdRng = match seed {
        Some(s) => StdRng::seed_from_u64(s),
        None    => StdRng::from_entropy(),
    };

    let n_markets = initial_state.markets.len();

    // Clone initial probability arrays (no mutation of the shared state).
    let mut market_probs:    Vec<f64> = initial_state.markets.iter().map(|m| m.probability).collect();
    let     implied_probs:   Vec<f64> = initial_state.markets.iter().map(|m| m.implied_prob).collect();
    let mut posterior_probs: Vec<f64> = initial_state.markets.iter().map(|m| m.posterior_prob).collect();

    let initial_value  = initial_state.portfolio_value;
    let mut equity     = initial_value;

    // Per-market open position: (entry_market_prob, is_buy, entry_pos_size).
    // Capturing the position size at entry avoids distortion from equity changes
    // while the position is open (mirrors the existing engine.rs approach).
    let mut positions: Vec<Option<(f64, bool, f64)>> = vec![None; n_markets];

    // Pre-allocate buffers.
    let mut equity_series: Vec<f64> = Vec::with_capacity(horizon_steps + 1);
    equity_series.push(initial_value);
    let mut tick_returns: Vec<f64>  = Vec::with_capacity(horizon_steps);

    for _step in 0..horizon_steps {
        // ── 1. Market evolution: Gaussian noise + mean reversion + drift ───────
        // Pre-generate all z values before mutably borrowing market_probs.
        let z_values: Vec<f64> = (0..n_markets).map(|_| sample_normal(&mut rng)).collect();

        for i in 0..n_markets {
            let noise  = config.volatility * z_values[i];
            let rev    = config.mean_reversion_speed * (implied_probs[i] - market_probs[i]);
            let delta  = noise + rev + config.drift;
            market_probs[i] = (market_probs[i] + delta).clamp(0.0, 1.0);
        }

        // ── 2. Shock injection (Scenario Engine integration point) ─────────────
        // Issue 5: sample shocked markets without replacement via choose_multiple.
        if config.shock_probability > 0.0 && rng.gen::<f64>() < config.shock_probability {
            let n_shocked = rng.gen_range(1..=n_markets);
            let all_indices: Vec<usize> = (0..n_markets).collect();
            let shocked: Vec<usize> = all_indices
                .choose_multiple(&mut rng, n_shocked)
                .copied()
                .collect();
            for idx in shocked {
                let sign = if rng.gen::<bool>() { 1.0_f64 } else { -1.0_f64 };
                let mag  = rng.gen::<f64>() * config.shock_magnitude;
                market_probs[idx] = (market_probs[idx] + sign * mag).clamp(0.0, 1.0);
            }
        }

        // ── 3. Bayesian posterior update (simplified batch_update_posteriors) ──
        // Blends the evolving market price into the posterior with 80/20 smoothing,
        // corresponding to fusing a weak market-price observation each tick.
        for i in 0..n_markets {
            posterior_probs[i] = (0.80 * posterior_probs[i] + 0.20 * market_probs[i]).clamp(0.0, 1.0);
        }

        // ── 4. Strategy execution + Risk Engine + Execution Simulator ──────────
        // Exits are always processed regardless of equity so open positions are
        // never stuck open after equity hits zero (Issue 2 fix).
        // Entries are still guarded: no new positions when equity ≤ 0.
        for i in 0..n_markets {
            let edge   = posterior_probs[i] - market_probs[i];
            let half_t = config.signal_threshold * 0.5;

            match positions[i] {
                None => {
                    // Entry signal: only open when equity is positive and finite.
                    if equity.is_finite() && equity > 0.0 {
                        let pos_size = config.position_fraction * equity;
                        if edge >= config.signal_threshold {
                            positions[i] = Some((market_probs[i], true, pos_size));
                        } else if edge <= -config.signal_threshold {
                            positions[i] = Some((market_probs[i], false, pos_size));
                        }
                    }
                }
                Some((entry_prob, is_buy, entry_size)) => {
                    // Exit always processed regardless of equity.
                    let still_open = if is_buy { edge >= half_t } else { edge <= -half_t };
                    if !still_open {
                        let pnl = if is_buy {
                            (market_probs[i] - entry_prob) * entry_size
                        } else {
                            (entry_prob - market_probs[i]) * entry_size
                        };
                        equity += pnl;
                        positions[i] = None;
                    }
                }
            }
        }

        // Guard against non-finite equity (catastrophic loss or numerical issue).
        if !equity.is_finite() {
            equity = 0.0;
        }

        // ── 5. Portfolio tracking ──────────────────────────────────────────────
        // Capture prev before pushing so the return calculation does not depend
        // on push order (Issue 11 fix).
        let prev = *equity_series.last().unwrap_or(&initial_value);
        equity_series.push(equity);
        let ret = if prev.abs() > f64::EPSILON { (equity - prev) / prev } else { 0.0 };
        tick_returns.push(ret);
    }

    SimulationPathResult {
        final_value: equity,
        returns:     tick_returns,
        // Issue 10: delegate to the shared helper instead of inline tracking.
        drawdown:    compute_max_drawdown(&equity_series),
    }
}

// ---------------------------------------------------------------------------
// Aggregation helpers
// ---------------------------------------------------------------------------

/// Aggregate per-path results into a single `MonteCarloResult`.
fn aggregate_results(paths: Vec<SimulationPathResult>, initial_value: f64, horizon_steps: usize) -> MonteCarloResult {
    if paths.is_empty() {
        return MonteCarloResult::default();
    }

    let n = paths.len() as f64;

    // Final return per path: (final_value - initial) / initial.
    let final_returns: Vec<f64> = paths
        .iter()
        .map(|p| {
            if initial_value > f64::EPSILON {
                (p.final_value - initial_value) / initial_value
            } else {
                0.0
            }
        })
        .collect();

    let expected_return = final_returns.iter().sum::<f64>() / n;
    let return_std      = compute_std(&final_returns, expected_return);
    let sharpe          = compute_sharpe_ratio(expected_return, return_std, horizon_steps);

    let max_drawdown = paths.iter().map(|p| p.drawdown).fold(0.0_f64, f64::max);

    let profit_probability = final_returns.iter().filter(|&&r| r > 0.0).count() as f64 / n;

    let (var_95, cvar_95) = compute_var_cvar(&final_returns, 0.95);

    MonteCarloResult {
        expected_return,
        return_std,
        sharpe_ratio: sharpe,
        max_drawdown,
        var_95,
        cvar_95,
        profit_probability,
        paths,
    }
}

// ---------------------------------------------------------------------------
// Public statistical helpers (also used directly in tests)
// ---------------------------------------------------------------------------

/// Compute Value-at-Risk and Conditional VaR (Expected Shortfall) at
/// `confidence_level` (e.g. `0.95` for 95%).
///
/// Both metrics are expressed as **positive loss fractions** of initial value.
///
/// * **VaR** — the loss at the `(1 - confidence_level)` quantile of the
///   return distribution. 95% VaR means "the loss will not exceed this
///   value in 95 out of 100 paths".
/// * **CVaR** — the mean loss in the worst `(1 - confidence_level)` paths.
///
/// Returns `(var, cvar)`.
pub fn compute_var_cvar(returns: &[f64], confidence_level: f64) -> (f64, f64) {
    if returns.is_empty() {
        return (0.0, 0.0);
    }

    let mut sorted = returns.to_vec();
    sorted.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let n = sorted.len();
    // Number of paths in the loss tail (at most n).
    let tail_size = ((1.0 - confidence_level) * n as f64).ceil() as usize;
    let tail_size = tail_size.clamp(1, n);

    // VaR: the loss at the tail boundary (flip sign: negative return → positive loss).
    let var = -sorted[tail_size - 1];

    // CVaR: mean of all losses in the tail (including the VaR boundary).
    let cvar = if tail_size > 1 {
        -sorted[..tail_size].iter().sum::<f64>() / tail_size as f64
    } else {
        var
    };

    (var.max(0.0), cvar.max(0.0))
}

/// Compute maximum peak-to-trough drawdown for an equity value series.
///
/// Returns a fraction ∈ `[0, 1]`: 0 = no drawdown, 1 = total loss.
pub fn compute_max_drawdown(equity_series: &[f64]) -> f64 {
    let mut peak  = f64::NEG_INFINITY;
    let mut max_dd = 0.0_f64;
    for &eq in equity_series {
        if eq > peak {
            peak = eq;
        } else if peak > 0.0 {
            let dd = ((peak - eq) / peak).min(1.0);
            if dd > max_dd {
                max_dd = dd;
            }
        }
    }
    max_dd
}

/// Compute the annualised Sharpe ratio from a mean return and its standard
/// deviation.
///
/// `horizon_steps` is the number of simulation ticks per path.  Assuming
/// 1-second ticks, the annualisation factor is √(TICKS_PER_YEAR / horizon_steps)
/// where TICKS_PER_YEAR = 252 trading days × 6.5 hours × 3 600 seconds.
///
/// Returns 0.0 when `std ≈ 0` or `horizon_steps == 0`.
pub fn compute_sharpe_ratio(mean_return: f64, std_return: f64, horizon_steps: usize) -> f64 {
    if std_return < f64::EPSILON || horizon_steps == 0 {
        return 0.0;
    }
    const TICKS_PER_YEAR: f64 = 252.0 * 6.5 * 3_600.0;
    let ann_factor = (TICKS_PER_YEAR / horizon_steps as f64).sqrt();
    mean_return / std_return * ann_factor
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Sample standard deviation of `values` given their precomputed `mean`.
/// Returns 0.0 when `values.len() < 2`.
fn compute_std(values: &[f64], mean: f64) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let n   = values.len() as f64;
    let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1.0);
    var.sqrt()
}

/// Draw one N(0,1) variate using `rand_distr::StandardNormal`.
///
/// Replaces the custom Box-Muller implementation, which discarded the second
/// variate Z2 on every call (Issue 8 fix).
fn sample_normal(rng: &mut StdRng) -> f64 {
    rng.sample(StandardNormal)
}
