// crates/bayesian_engine/src/types.rs
// Core domain types for the Bayesian Probability Engine.

use std::collections::VecDeque;
use std::time::Instant;

use serde::{Deserialize, Serialize};

// ── Evidence source ────────────────────────────────────────────────────────────

/// The origin of a piece of evidence ingested into a [`BeliefState`].
///
/// Used to classify history entries and to weight evidence differently in
/// strategy logic downstream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EvidenceSource {
    /// Live market price observed from Polymarket / Kalshi.
    MarketPrice,
    /// News article sentiment processed by the news agent.
    NewsSentiment,
    /// Aggregated polling data (e.g. from 538 or Metaculus).
    PollAggregate,
    /// Macro-economic indicator (e.g. CPI, unemployment, yield curve).
    MacroIndicator,
    /// Probability override entered manually by an analyst.
    ManualOverride,
    /// Implied-probability change propagated from an upstream graph node.
    GraphPropagation,
    /// Any other labelled source.
    Other(String),
}

// ── Evidence history entry ─────────────────────────────────────────────────────

/// A single evidence contribution recorded in [`BeliefState::update_history`].
#[derive(Debug, Clone)]
pub struct EvidenceEntry {
    /// Wall-clock timestamp of the update.
    pub timestamp: Instant,
    /// Signed log-odds that was actually applied (= raw delta × strength).
    pub log_odds_applied: f64,
    /// Where this evidence came from.
    pub source: EvidenceSource,
}

// ── Belief state ──────────────────────────────────────────────────────────────

/// Maximum number of [`EvidenceEntry`] records kept per market.
///
/// When the cap is reached the oldest entry is dropped (FIFO).
pub(crate) const MAX_HISTORY: usize = 100;

/// Per-market belief state maintained by the [`BayesianEngine`].
///
/// # Posterior computation
///
/// ```text
/// prior_log_odds         = ln(prior_prob / (1 − prior_prob))
/// total_log_odds         = prior_log_odds + evidence_log_odds + market_fusion_log_odds
/// posterior_prob         = 1 / (1 + exp(−total_log_odds))
/// ```
///
/// * `evidence_log_odds` is **accumulated** across multiple
///   [`ingest_evidence`](crate::BayesianEngine::ingest_evidence) calls.
/// * `market_fusion_log_odds` is **replaced** (not accumulated) on each
///   [`fuse_market_price`](crate::BayesianEngine::fuse_market_price) call, so
///   a new market price always completely supersedes the previous one.
pub struct BeliefState {
    /// Unique market identifier.
    pub market_id: String,

    /// Model's baseline probability before any evidence [0.0, 1.0].
    ///
    /// Initialised to the first market price seen; can be updated explicitly
    /// via [`BayesianEngine::update_prior`].
    pub prior_prob: f64,

    /// Most recently cached posterior probability [0.0, 1.0].
    ///
    /// Kept up-to-date by every mutation method.  Call
    /// [`BayesianEngine::compute_posterior`] to force a recompute.
    pub posterior_prob: f64,

    /// Accumulated log-odds from all non-market evidence sources.
    ///
    /// This grows with each [`ingest_evidence`](crate::BayesianEngine::ingest_evidence)
    /// call and is never reset automatically.  Use [`BayesianEngine::reset_evidence`]
    /// to start fresh.
    pub evidence_log_odds: f64,

    /// Log-odds contribution from the most recent market price fusion.
    ///
    /// Unlike `evidence_log_odds`, this field is **replaced** rather than
    /// accumulated: calling `fuse_market_price` twice only counts the price
    /// once (the most recent observation).
    ///
    /// Formula: `market_precision × (log_odds(market_prob) − log_odds(prior_prob))`
    pub market_fusion_log_odds: f64,

    /// Most recently observed market price, if any.
    pub market_prob: Option<f64>,

    /// Confidence weight for the market price in [0.0, 1.0].
    ///
    /// Higher values mean the market price pulls the posterior closer to it.
    /// Set by [`BayesianEngine::fuse_market_price`] via the `market_precision`
    /// argument.
    pub market_confidence: f64,

    /// Monotonic timestamp of the most recent update.
    pub last_update: Instant,

    /// Ordered history of evidence contributions (capped at [`MAX_HISTORY`]).
    ///
    /// Stored as a [`VecDeque`] so that FIFO eviction is O(1).
    pub update_history: VecDeque<EvidenceEntry>,
}

impl BeliefState {
    /// Create a fresh belief state with `initial_prob` as the prior.
    ///
    /// All accumulated log-odds start at zero; the posterior equals the prior.
    pub(crate) fn new(market_id: String, initial_prob: f64) -> Self {
        let p = initial_prob.clamp(0.0, 1.0);
        Self {
            market_id,
            prior_prob: p,
            posterior_prob: p,
            evidence_log_odds: 0.0,
            market_fusion_log_odds: 0.0,
            market_prob: None,
            market_confidence: 0.0,
            last_update: Instant::now(),
            update_history: VecDeque::new(),
        }
    }

    /// Compute the total posterior log-odds from the stored components.
    #[inline]
    pub(crate) fn total_log_odds(&self) -> f64 {
        prob_to_log_odds(self.prior_prob)
            + self.evidence_log_odds
            + self.market_fusion_log_odds
    }

    /// Recompute `posterior_prob` from the current log-odds components and
    /// cache the result.  Returns the new posterior.
    #[inline]
    pub(crate) fn recompute(&mut self) -> f64 {
        let p = log_odds_to_prob(self.total_log_odds());
        self.posterior_prob = p;
        self.last_update = Instant::now();
        p
    }
}

// ── Signal + Kelly result ─────────────────────────────────────────────────────

/// Return value of [`BayesianEngine::get_signal_and_kelly`].
///
/// # Formulas
///
/// ```text
/// p  = posterior_prob (clamped to [0.001, 0.999])
/// m  = market_price   (clamped to [0.001, 0.999])
/// c  = model_confidence = sqrt(history_len / MAX_HISTORY)  [0.0, 1.0]
///
/// edge            = logit(p) − logit(m)
/// signal_strength = edge × c
///
/// YES Kelly (p > m):  kelly_raw = (p − m) / (1 − m)
/// NO  Kelly (p < m):  kelly_raw = −(m − p) / m          ← correct NO sizing
/// kelly_fraction      = clamp(kelly_raw, −1, 1) × c
/// ```
///
/// # Example (64 history entries → c = 0.80)
///
/// ```text
/// posterior = 0.65, market = 0.55, c = 0.80
///
/// edge            = logit(0.65) − logit(0.55) ≈ 0.4183
/// signal_strength = 0.4183 × 0.80 ≈ 0.3347
///
/// kelly_raw (YES) = (0.65 − 0.55) / (1 − 0.55) ≈ 0.2222
/// kelly_fraction  = 0.2222 × 0.80 ≈ 0.1778
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct SignalResult {
    /// Log-odds edge between model posterior and market price, scaled by
    /// `model_confidence`.
    ///
    /// * Positive → model assigns higher probability than the market → potential **BUY**.
    /// * Negative → model assigns lower probability than the market → potential **SELL / NO**.
    pub signal_strength: f64,

    /// Kelly-optimal fraction of bankroll to allocate, adjusted by confidence.
    ///
    /// Clamped to `[−1.0, 1.0]`.  Negative values indicate a short / NO position.
    /// Multiply by a fractional-Kelly factor (e.g. 0.25) before sizing real orders.
    pub kelly_fraction: f64,
}

// ── Log-odds math helpers ─────────────────────────────────────────────────────

/// Convert a probability to log-odds: `ln(p / (1 − p))`.
///
/// Clamps `p` to `[1e-9, 1 − 1e-9]` to avoid ±∞.
#[inline]
pub(crate) fn prob_to_log_odds(p: f64) -> f64 {
    let p = p.clamp(1e-9, 1.0 - 1e-9);
    (p / (1.0 - p)).ln()
}

/// Logistic (sigmoid) function: `1 / (1 + exp(−x))`.
///
/// Maps any log-odds value back to a probability in `(0, 1)`.
#[inline]
pub(crate) fn log_odds_to_prob(lo: f64) -> f64 {
    1.0 / (1.0 + (-lo).exp())
}

/// First-order Fisher-information approximation: convert a probability *delta*
/// to its log-odds equivalent using `reference_prob` as the operating point.
///
/// ```text
/// Δlog_odds ≈ Δp / (p · (1 − p))
/// ```
///
/// At `p = 0.5` this gives `4 × Δp`; the approximation degrades near 0 or 1.
/// `reference_prob` is clamped to `[0.01, 0.99]` to bound the output.
#[inline]
pub(crate) fn delta_prob_to_log_odds(delta: f64, reference_prob: f64) -> f64 {
    let p = reference_prob.clamp(0.01, 0.99);
    delta / (p * (1.0 - p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;

    const EPS: f64 = 1e-9;

    #[test]
    fn prob_log_odds_round_trip() {
        for p in [0.1, 0.3, 0.5, 0.7, 0.9] {
            assert_abs_diff_eq!(log_odds_to_prob(prob_to_log_odds(p)), p, epsilon = EPS);
        }
    }

    #[test]
    fn belief_state_initial_posterior_equals_prior() {
        let b = BeliefState::new("test".to_string(), 0.6);
        assert_abs_diff_eq!(b.posterior_prob, 0.6, epsilon = EPS);
        assert_abs_diff_eq!(b.prior_prob, 0.6, epsilon = EPS);
        assert_eq!(b.evidence_log_odds, 0.0);
        assert_eq!(b.market_fusion_log_odds, 0.0);
    }
}
