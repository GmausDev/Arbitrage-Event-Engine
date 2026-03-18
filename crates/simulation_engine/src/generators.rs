// crates/simulation_engine/src/generators.rs
//
// Synthetic market probability generators.
//
// `MarketGenerator` produces correlated random-walk paths for an arbitrary
// set of synthetic markets.  Correlation is implemented via a common-factor
// model: each market's per-tick innovation is a weighted blend of a shared
// Gaussian factor (providing correlation) and an idiosyncratic factor.

use std::f64::consts::PI;

use chrono::Utc;
use common::{MarketNode, MarketUpdate};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::{
    config::{MarketSpec, MonteCarloConfig},
    types::SimulationTick,
};

// ---------------------------------------------------------------------------
// MarketGenerator
// ---------------------------------------------------------------------------

/// Generates successive `SimulationTick`s for a set of synthetic markets.
///
/// # Probability model
///
/// For each tick the per-market innovation is:
/// ```text
/// dW_i = sqrt(ρ) · Z_common + sqrt(1 - ρ) · Z_i
/// p_i(t+1) = clamp(p_i(t) + drift + σ · dW_i, 0.01, 0.99)
/// ```
/// where `Z_common` and each `Z_i` are i.i.d. N(0, 1) drawn via Box-Muller.
/// This gives `Var(dW_i) = ρ + (1-ρ) = 1` and `Corr(dW_i, dW_j) = ρ` for
/// all `i ≠ j`.  Negative `ρ` is clamped to 0; arbitrary negative pairwise
/// correlation requires Cholesky decomposition of the full correlation matrix.
pub struct MarketGenerator {
    rng:       StdRng,
    /// Current state: paired (spec, current_probability).
    markets:   Vec<(MarketSpec, f64)>,
    mc_config: MonteCarloConfig,
    tick:      usize,
}

impl MarketGenerator {
    /// Create a new generator.  If `mc_config.seed` is `Some`, the RNG is
    /// deterministic; otherwise it is seeded from entropy.
    pub fn new(markets: Vec<MarketSpec>, mc_config: MonteCarloConfig) -> Self {
        let rng = match mc_config.seed {
            Some(s) => StdRng::seed_from_u64(s),
            None    => StdRng::from_entropy(),
        };
        let state = markets.iter().map(|m| (m.clone(), m.initial_prob)).collect();
        Self { rng, markets: state, mc_config, tick: 0 }
    }

    /// Reset all market probabilities to their `initial_prob` values and set
    /// the tick counter back to zero.  The RNG state is **not** reset, so
    /// subsequent paths are independent from prior ones when using a seed.
    pub fn reset(&mut self) {
        for (spec, prob) in &mut self.markets {
            *prob = spec.initial_prob;
        }
        self.tick = 0;
    }

    /// Advance the generator by one tick and return the resulting
    /// `SimulationTick` containing one `MarketUpdate` per market.
    pub fn next_tick(&mut self) -> SimulationTick {
        let now       = Utc::now();
        let timestamp = now.timestamp_millis();
        let rho       = self.mc_config.correlation.clamp(-1.0, 1.0);

        // Clamp negative correlation to 0: arbitrary negative pairwise
        // correlation requires Cholesky decomposition of the correlation matrix.
        let rho_clamped = rho.max(0.0);
        let sqrt_rho    = rho_clamped.sqrt();
        // Residual weight: Var(dW_i) = rho + (1 - rho) = 1.
        let sqrt_res = (1.0 - rho_clamped).sqrt();

        // Pre-generate all random samples before borrowing markets mutably.
        // (sample_normal borrows self.rng; the loop below borrows self.markets.)
        let z_common = sample_normal(&mut self.rng);
        let z_idios: Vec<f64> = (0..self.markets.len())
            .map(|_| sample_normal(&mut self.rng))
            .collect();

        let drift = self.mc_config.drift;
        let vol   = self.mc_config.volatility;
        let mut updates = Vec::with_capacity(self.markets.len());

        for ((spec, prob), z_idio) in self.markets.iter_mut().zip(z_idios) {
            let dz = sqrt_rho * z_common + sqrt_res * z_idio;
            let dp = drift + vol * dz;
            *prob  = (*prob + dp).clamp(0.01, 0.99);

            updates.push(MarketUpdate {
                market: MarketNode {
                    id:          spec.id.clone(),
                    probability: *prob,
                    liquidity:   spec.liquidity,
                    last_update: now,
                },
            });
        }

        self.tick += 1;
        SimulationTick { timestamp, market_updates: updates }
    }

    /// Apply an instantaneous shock to all markets.  Each market shifts by
    /// `±magnitude` (direction chosen independently at random).  Useful for
    /// simulating news events within a path.
    pub fn apply_shock(&mut self, magnitude: f64) {
        // Pre-generate signs to avoid borrowing self.rng inside the loop that
        // borrows self.markets.
        let signs: Vec<f64> = (0..self.markets.len())
            .map(|_| if self.rng.gen::<bool>() { 1.0 } else { -1.0 })
            .collect();
        for ((_spec, prob), sign) in self.markets.iter_mut().zip(signs) {
            *prob = (*prob + sign * magnitude).clamp(0.01, 0.99);
        }
    }

    /// Current tick index (number of ticks produced so far).
    pub fn tick_count(&self) -> usize {
        self.tick
    }

    /// Current probability for the market at `index`, or `None` if out of range.
    pub fn current_prob(&self, index: usize) -> Option<f64> {
        self.markets.get(index).map(|(_, p)| *p)
    }

}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Box-Muller transform: draw one N(0, 1) sample from `rng`.
fn sample_normal(rng: &mut StdRng) -> f64 {
    let u1: f64 = rng.gen_range(f64::EPSILON..1.0);
    let u2: f64 = rng.gen::<f64>();
    (-2.0 * u1.ln()).sqrt() * (2.0 * PI * u2).cos()
}
