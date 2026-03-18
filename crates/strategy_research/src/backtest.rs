// crates/strategy_research/src/backtest.rs
//
// Self-contained synthetic backtest runner.
//
// Simulation model
// ─────────────────
// • N correlated random-walk markets using a common-factor model:
//       Δp_i = drift + vol × (√ρ × Z_common + √(1−ρ) × Z_i)
//   where Z_common and Z_i are independent N(0,1) draws (Box-Muller).
//
// • Signal features computed from the synthetic paths:
//     PosteriorEdge      — posterior_noise ~ N(0, 0.025) added to market_prob
//     GraphArbitrageEdge — 30 % blend with neighbour market probability
//     TemporalMomentum   — rolling z-score of price deltas (window = 20)
//     ShockSignal        — |Δp| > 2σ → magnitude ∈ [0, 1]
//     MarketVolatility   — rolling std-dev of price deltas
//
// • Position model:
//     - At most one position open per market at a time.
//     - Position opened when strategy.evaluate() returns Some((dir, frac)).
//     - Position force-closed after `hold_ticks` ticks.
//     - PnL = pnl_fraction × position_fraction × initial_capital  (no compounding).

use crate::{
    config::ResearchConfig,
    types::{BacktestResult, BacktestTrade, GeneratedStrategy, MarketSnapshot},
};
use common::TradeDirection;
use rand::{rngs::SmallRng, Rng, SeedableRng};
use std::collections::VecDeque;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const ROLLING_WINDOW: usize = 20;
const MIN_HISTORY_FOR_ZSCORE: usize = 5;
/// A price delta is a "shock" when its z-score exceeds this value.
const SHOCK_Z_THRESHOLD: f64 = 2.0;
/// Standard deviation of the synthetic Bayesian posterior noise.
const POSTERIOR_NOISE_STD: f64 = 0.025;

// ---------------------------------------------------------------------------
// Internal per-market state
// ---------------------------------------------------------------------------

struct MarketState {
    id: String,
    prob: f64,
    delta_history: VecDeque<f64>,
    shock_magnitude: f64,
}

impl MarketState {
    fn new(id: String, initial_prob: f64) -> Self {
        Self {
            id,
            prob: initial_prob,
            delta_history: VecDeque::with_capacity(ROLLING_WINDOW + 1),
            shock_magnitude: 0.0,
        }
    }

    fn push_delta(&mut self, delta: f64) {
        self.delta_history.push_back(delta);
        if self.delta_history.len() > ROLLING_WINDOW {
            self.delta_history.pop_front();
        }
    }

    fn mean_delta(&self) -> f64 {
        if self.delta_history.is_empty() {
            return 0.0;
        }
        self.delta_history.iter().sum::<f64>() / self.delta_history.len() as f64
    }

    fn stddev_delta(&self) -> f64 {
        if self.delta_history.len() < 2 {
            return 1e-6;
        }
        let mean = self.mean_delta();
        let var = self
            .delta_history
            .iter()
            .map(|d| (d - mean).powi(2))
            .sum::<f64>()
            / (self.delta_history.len() - 1) as f64;
        var.sqrt().max(1e-6)
    }

    fn temporal_z_score(&self, latest_delta: f64) -> f64 {
        if self.delta_history.len() < MIN_HISTORY_FOR_ZSCORE {
            return 0.0;
        }
        (latest_delta - self.mean_delta()) / self.stddev_delta()
    }

    fn volatility(&self) -> f64 {
        self.stddev_delta()
    }
}

// ---------------------------------------------------------------------------
// Open position tracker
// ---------------------------------------------------------------------------

struct OpenPosition {
    direction: TradeDirection,
    fraction: f64,
    entry_prob: f64,
    ticks_held: usize,
}

// ---------------------------------------------------------------------------
// Box-Muller N(0,1) sampler
// ---------------------------------------------------------------------------

fn sample_normal(rng: &mut SmallRng) -> f64 {
    let u1 = rng.gen::<f64>().max(1e-15_f64);
    let u2 = rng.gen::<f64>();
    (-2.0_f64 * u1.ln()).sqrt() * (2.0_f64 * std::f64::consts::PI * u2).cos()
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Run a full synthetic backtest for one `GeneratedStrategy`.
///
/// CPU-bound; intended to be called from `tokio::task::spawn_blocking`.
pub fn run_backtest(strategy: &GeneratedStrategy, config: &ResearchConfig, seed: u64) -> BacktestResult {
    let mut rng = SmallRng::seed_from_u64(seed);

    // Initialise markets at random starting probabilities in [0.20, 0.80].
    let n = config.backtest_markets;
    let mut markets: Vec<MarketState> = (0..n)
        .map(|i| {
            let prob = rng.gen_range(0.20_f64..=0.80);
            MarketState::new(format!("market_{i}"), prob)
        })
        .collect();

    let mut positions: Vec<Option<OpenPosition>> = (0..n).map(|_| None).collect();
    let mut trades: Vec<BacktestTrade> = Vec::new();
    let mut realised_pnl = 0.0_f64;
    let mut equity_curve: Vec<f64> = Vec::with_capacity(config.backtest_ticks);

    let rho = config.backtest_correlation.clamp(0.0, 1.0);
    let sqrt_rho = rho.sqrt();
    let sqrt_one_minus_rho = (1.0 - rho).sqrt();

    for _tick in 0..config.backtest_ticks {
        // ── Step 1: advance all market probabilities ─────────────────────
        let common_z = sample_normal(&mut rng);
        let mut new_probs = vec![0.0_f64; n];
        let mut actual_deltas = vec![0.0_f64; n];
        let mut z_scores = vec![0.0_f64; n];

        for i in 0..n {
            let idio_z = sample_normal(&mut rng);
            let z = sqrt_rho * common_z + sqrt_one_minus_rho * idio_z;
            let raw_delta = config.backtest_drift + config.backtest_volatility * z;
            let new_prob = (markets[i].prob + raw_delta).clamp(0.01, 0.99);
            let actual_delta = new_prob - markets[i].prob;

            z_scores[i] = markets[i].temporal_z_score(actual_delta);
            new_probs[i] = new_prob;
            actual_deltas[i] = actual_delta;
        }

        for i in 0..n {
            markets[i].shock_magnitude = if z_scores[i].abs() >= SHOCK_Z_THRESHOLD {
                (z_scores[i].abs() / (SHOCK_Z_THRESHOLD * 3.0)).clamp(0.0, 1.0)
            } else {
                0.0
            };
            markets[i].push_delta(actual_deltas[i]);
            markets[i].prob = new_probs[i];
        }

        // ── Step 2: close mature positions ───────────────────────────────
        for i in 0..n {
            let should_close = positions[i]
                .as_ref()
                .map_or(false, |p| p.ticks_held >= config.hold_ticks);

            if should_close {
                let pos = positions[i].take().unwrap();
                let exit_prob = markets[i].prob;
                let pnl_fraction = pnl_frac(pos.direction, pos.entry_prob, exit_prob);
                let pnl_abs = pnl_fraction * pos.fraction * config.initial_capital;

                trades.push(BacktestTrade {
                    market_id: markets[i].id.clone(),
                    direction: pos.direction,
                    position_fraction: pos.fraction,
                    entry_prob: pos.entry_prob,
                    exit_prob,
                    pnl_fraction,
                    pnl_abs,
                });
                realised_pnl += pnl_abs;
            } else if let Some(pos) = positions[i].as_mut() {
                pos.ticks_held += 1;
            }
        }

        // ── Step 3: evaluate strategy and open new positions ─────────────
        for i in 0..n {
            if positions[i].is_some() {
                continue; // already holding
            }

            // Graph-implied probability: weighted blend with next market.
            let neighbour_idx = (i + 1) % n;
            let neighbour_prob = markets[neighbour_idx].prob;
            let implied_graph_prob = (0.70 * markets[i].prob + 0.30 * neighbour_prob).clamp(0.01, 0.99);

            // Synthetic Bayesian posterior: market_prob + Gaussian model noise.
            let posterior_noise = POSTERIOR_NOISE_STD * sample_normal(&mut rng);
            let posterior_prob = (markets[i].prob + posterior_noise).clamp(0.01, 0.99);

            // Reuse the z-score computed in Step 1 (before delta was pushed into history)
            // to avoid self-inclusion bias from re-computing after the push.
            let temporal_z = z_scores[i];

            let snapshot = MarketSnapshot {
                market_id: markets[i].id.clone(),
                market_prob: markets[i].prob,
                posterior_edge: posterior_prob - markets[i].prob,
                graph_arb_edge: implied_graph_prob - markets[i].prob,
                temporal_z_score: temporal_z,
                shock_magnitude: markets[i].shock_magnitude,
                volatility: markets[i].volatility(),
                posterior_prob,
                implied_graph_prob,
            };

            if let Some((direction, fraction)) = strategy.evaluate(&snapshot) {
                positions[i] = Some(OpenPosition {
                    direction,
                    fraction,
                    entry_prob: markets[i].prob,
                    ticks_held: 0,
                });
            }
        }

        // ── Record equity (realised only; no MTM to keep allocator simple) ─
        equity_curve.push(config.initial_capital + realised_pnl);
    }

    // ── Force-close all remaining positions at final prices ───────────────
    for i in 0..n {
        if let Some(pos) = positions[i].take() {
            let exit_prob = markets[i].prob;
            let pnl_fraction = pnl_frac(pos.direction, pos.entry_prob, exit_prob);
            let pnl_abs = pnl_fraction * pos.fraction * config.initial_capital;

            trades.push(BacktestTrade {
                market_id: markets[i].id.clone(),
                direction: pos.direction,
                position_fraction: pos.fraction,
                entry_prob: pos.entry_prob,
                exit_prob,
                pnl_fraction,
                pnl_abs,
            });
            // Keep realised_pnl in sync so equity_curve reflects these trades.
            realised_pnl += pnl_abs;
        }
    }

    // Update the final equity point to include force-closed PnL so that
    // equity-level metrics (Sharpe, total_return) match trade-level metrics.
    if let Some(last) = equity_curve.last_mut() {
        *last = config.initial_capital + realised_pnl;
    }

    BacktestResult {
        strategy_id: strategy.id,
        trades,
        equity_curve,
        initial_capital: config.initial_capital,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Signed PnL fraction: positive = profit, negative = loss.
fn pnl_frac(direction: TradeDirection, entry: f64, exit: f64) -> f64 {
    match direction {
        TradeDirection::Buy => exit - entry,
        TradeDirection::Sell => entry - exit,
        TradeDirection::Arbitrage => exit - entry,
    }
}
