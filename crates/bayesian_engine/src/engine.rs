// crates/bayesian_engine/src/engine.rs
// BayesianEngine — synchronous core of the Bayesian Probability Engine.
//
// Thread safety: callers should wrap in `Arc<RwLock<BayesianEngine>>`.

use std::collections::HashMap;

use tracing::{debug, trace};

use crate::{
    error::BayesianError,
    types::{
        delta_prob_to_log_odds, log_odds_to_prob, prob_to_log_odds, BeliefState, EvidenceEntry,
        EvidenceSource, SignalResult, MAX_HISTORY,
    },
};
use market_graph::MarketGraphEngine;

// ── Numerical helpers ─────────────────────────────────────────────────────────

/// Clamp `p` to `[0.001, 0.999]` and replace NaN with 0.5.
///
/// The bound `0.001` is intentionally wider than the `[1e-9, 1-1e-9]` used by
/// [`prob_to_log_odds`]: Kelly payout odds involve `1/m` and `1/(1-m)`, and
/// allowing `m` as extreme as `1e-9` would produce odds of `10^9` — numerically
/// valid but economically meaningless for a prediction market.  `0.001`
/// limits payout odds to at most `999×`, a reasonable real-world ceiling.
#[inline]
fn clamp_probability(p: f64) -> f64 {
    if p.is_nan() {
        return 0.5;
    }
    p.clamp(0.001, 0.999)
}

/// Signed Kelly fraction for a binary prediction-market contract.
///
/// Dispatches to the correct formula based on the position direction:
///
/// ```text
/// YES (p > m):  kelly =  (p − m) / (1 − m)   [derived: (b·p − q) / b, b = (1-m)/m]
/// NO  (p < m):  kelly = −(m − p) / m          [derived: −(b·q − p) / b, b = m/(1-m)]
/// equal:        kelly =  0.0
/// ```
///
/// Using the symmetric NO formula when `p < m` gives the correct position size
/// for a short/NO bet.  Treating it as a negative YES Kelly underestimates the
/// NO fraction when `m > 0.5` and overestimates it when `m < 0.5`.
///
/// Output is **not** clamped here; clamping to `[−1.0, 1.0]` is done at the
/// call site so that the raw value can be logged or inspected separately.
#[inline]
fn compute_kelly(p: f64, market_price: f64) -> f64 {
    let m = market_price;
    if p > m {
        // YES / buy position.  Net payout odds b = (1-m)/m.
        let denom = 1.0 - m;
        if denom < 1e-12 { return 0.0; }
        (p - m) / denom
    } else if p < m {
        // NO / short position.  Net payout odds b_no = m/(1-m).
        // Correct NO Kelly = (m-p)/m; negate to indicate short direction.
        if m < 1e-12 { return 0.0; }
        -(m - p) / m
    } else {
        0.0
    }
}

// ── Validation helpers ────────────────────────────────────────────────────────

#[inline]
fn validate_prob(p: f64) -> Result<(), BayesianError> {
    if (0.0..=1.0).contains(&p) {
        Ok(())
    } else {
        Err(BayesianError::InvalidProbability(p))
    }
}

#[inline]
fn validate_precision(c: f64) -> Result<(), BayesianError> {
    if (0.0..=1.0).contains(&c) {
        Ok(())
    } else {
        Err(BayesianError::InvalidPrecision(c))
    }
}

// ── BayesianEngine ────────────────────────────────────────────────────────────

/// Synchronous Bayesian Probability Engine.
///
/// Maintains a [`BeliefState`] per market and fuses evidence from multiple
/// sources using log-odds arithmetic.
///
/// # Posterior formula
///
/// ```text
/// posterior_log_odds = log_odds(prior) + evidence_log_odds + market_fusion_log_odds
/// posterior          = sigmoid(posterior_log_odds)
/// ```
///
/// # Example
///
/// ```rust
/// use bayesian_engine::{BayesianEngine, EvidenceSource};
///
/// let mut engine = BayesianEngine::new();
/// engine.ensure_belief("trump_win", 0.50);
/// engine.update_prior("trump_win", 0.58).unwrap();
/// engine.fuse_market_price("trump_win", 0.54, 0.80).unwrap();
/// engine.ingest_evidence("trump_win", 0.35, EvidenceSource::NewsSentiment, 0.6).unwrap();
///
/// let post = engine.compute_posterior("trump_win").unwrap();
/// let dev  = engine.get_deviation_from_market("trump_win").unwrap();
/// println!("posterior={post:.3}  deviation={dev:+.3}");
/// ```
pub struct BayesianEngine {
    /// Per-market belief states, keyed by market ID.
    beliefs: HashMap<String, BeliefState>,
}

impl Default for BayesianEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl BayesianEngine {
    /// Create an empty engine with no pre-seeded beliefs.
    ///
    /// Beliefs are added lazily via [`ensure_belief`](Self::ensure_belief) or
    /// by the event-loop wrapper as `MarketUpdate` events arrive.
    pub fn new() -> Self {
        Self {
            beliefs: HashMap::new(),
        }
    }

    /// Create an engine pre-seeded from all markets currently in a
    /// [`MarketGraphEngine`].
    ///
    /// Each market's `current_prob` becomes the initial prior.  This is the
    /// recommended constructor when you have a populated graph at startup.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let graph = MarketGraphEngine::new(); // populated elsewhere
    /// let mut bayes = BayesianEngine::from_graph(&graph);
    /// bayes.update_prior("trump_win", 0.58).unwrap();
    /// ```
    pub fn from_graph(graph: &MarketGraphEngine) -> Self {
        let mut engine = Self::new();
        for id in graph.market_ids() {
            if let Ok(node) = graph.get_market(id) {
                engine.ensure_belief(id, node.current_prob);
            }
        }
        engine
    }

    // ── Belief management ─────────────────────────────────────────────────────

    /// Return a mutable reference to the [`BeliefState`] for `market_id`,
    /// creating it with `initial_prob` as the prior if it does not yet exist.
    ///
    /// This is the preferred way to initialise a belief from a `MarketUpdate`:
    /// the market price is used as the starting prior and no evidence is fused.
    pub fn ensure_belief(&mut self, market_id: &str, initial_prob: f64) -> &mut BeliefState {
        self.beliefs
            .entry(market_id.to_string())
            .or_insert_with(|| {
                debug!(market_id, initial_prob, "bayesian_engine: new belief state created");
                BeliefState::new(market_id.to_string(), initial_prob.clamp(0.0, 1.0))
            })
    }

    /// Immutable access to the [`BeliefState`] for `market_id`, or `None` if
    /// the market has not yet been seen.
    #[must_use]
    pub fn get_belief(&self, market_id: &str) -> Option<&BeliefState> {
        self.beliefs.get(market_id)
    }

    /// All market IDs currently tracked by the engine.
    #[must_use]
    pub fn market_ids(&self) -> Vec<&str> {
        self.beliefs.keys().map(String::as_str).collect()
    }

    /// Number of markets with active belief states.
    #[must_use]
    pub fn belief_count(&self) -> usize {
        self.beliefs.len()
    }

    // ── Core operations ───────────────────────────────────────────────────────

    /// Update the prior probability for `market_id`.
    ///
    /// The prior is the model's baseline belief before any evidence.  Changing
    /// it shifts the posterior immediately; accumulated evidence is preserved.
    ///
    /// If a market price was previously fused, the `market_fusion_log_odds`
    /// component is recalculated against the new prior so the relative pull
    /// of the market price remains consistent.
    ///
    /// # Errors
    /// Returns [`BayesianError::MarketNotFound`] if the market has no belief
    /// state yet; call [`ensure_belief`](Self::ensure_belief) first.
    /// Returns [`BayesianError::InvalidProbability`] if `new_prior ∉ [0, 1]`.
    pub fn update_prior(
        &mut self,
        market_id: &str,
        new_prior: f64,
    ) -> Result<(), BayesianError> {
        validate_prob(new_prior)?;
        let belief = self
            .beliefs
            .get_mut(market_id)
            .ok_or_else(|| BayesianError::MarketNotFound(market_id.to_string()))?;

        let old_prior = belief.prior_prob;
        belief.prior_prob = new_prior;

        // Re-derive the market fusion component against the new prior so the
        // market-price pull is measured relative to the updated baseline.
        if let Some(mp) = belief.market_prob {
            let market_lo = prob_to_log_odds(mp);
            let prior_lo  = prob_to_log_odds(new_prior);
            belief.market_fusion_log_odds =
                belief.market_confidence * (market_lo - prior_lo);
        }

        belief.recompute();

        debug!(
            market_id,
            old_prior,
            new_prior,
            posterior = belief.posterior_prob,
            "bayesian_engine: prior updated"
        );
        Ok(())
    }

    /// Ingest a piece of evidence as a signed log-odds delta.
    ///
    /// The contribution added to `evidence_log_odds` is `log_odds_delta × strength`.
    ///
    /// | `log_odds_delta` sign | Meaning                              |
    /// |-----------------------|--------------------------------------|
    /// | Positive              | Evidence that the event *will* happen |
    /// | Negative              | Evidence that the event *won't* happen |
    ///
    /// `strength` is a multiplier in the caller's control — e.g. 0.6 means
    /// "I trust this source at 60 %".  Values > 1.0 are allowed (amplification).
    ///
    /// Evidence accumulates: calling this method N times with the same delta
    /// applies the delta N times.  Use `reset_evidence` to start fresh.
    ///
    /// # Errors
    /// Returns [`BayesianError::MarketNotFound`] if the market has no belief state.
    pub fn ingest_evidence(
        &mut self,
        market_id: &str,
        log_odds_delta: f64,
        source: EvidenceSource,
        strength: f64,
    ) -> Result<(), BayesianError> {
        let belief = self
            .beliefs
            .get_mut(market_id)
            .ok_or_else(|| BayesianError::MarketNotFound(market_id.to_string()))?;

        let applied = log_odds_delta * strength;
        belief.evidence_log_odds += applied;

        // Rotate history FIFO — oldest entry dropped when cap is reached (O(1)).
        if belief.update_history.len() >= MAX_HISTORY {
            belief.update_history.pop_front();
        }
        belief.update_history.push_back(EvidenceEntry {
            timestamp: std::time::Instant::now(),
            log_odds_applied: applied,
            source,
        });

        belief.recompute();

        trace!(
            market_id,
            log_odds_delta,
            strength,
            applied,
            new_evidence_log_odds = belief.evidence_log_odds,
            posterior = belief.posterior_prob,
            "bayesian_engine: evidence ingested"
        );
        Ok(())
    }

    /// Fuse an observed market price into the posterior using precision weighting.
    ///
    /// The market price contribution is stored in a dedicated
    /// `market_fusion_log_odds` component that **replaces** the previous value
    /// on each call (rather than accumulating), so a new price update always
    /// supersedes the old one.
    ///
    /// # Fusion formula
    ///
    /// ```text
    /// market_fusion_log_odds = market_precision × (log_odds(market_prob) − log_odds(prior_prob))
    /// ```
    ///
    /// * At `market_precision = 1.0` the posterior fully adopts the market price
    ///   (subject to any other accumulated evidence).
    /// * At `market_precision = 0.0` the market price has no effect.
    ///
    /// # Errors
    /// Returns [`BayesianError::MarketNotFound`] if the market has no belief state.
    /// Returns [`BayesianError::InvalidProbability`] or [`BayesianError::InvalidPrecision`]
    /// on out-of-range inputs.
    pub fn fuse_market_price(
        &mut self,
        market_id: &str,
        market_prob: f64,
        market_precision: f64,
    ) -> Result<(), BayesianError> {
        validate_prob(market_prob)?;
        validate_precision(market_precision)?;
        let belief = self
            .beliefs
            .get_mut(market_id)
            .ok_or_else(|| BayesianError::MarketNotFound(market_id.to_string()))?;

        let market_lo = prob_to_log_odds(market_prob);
        let prior_lo  = prob_to_log_odds(belief.prior_prob);

        // Replace (not accumulate) the market fusion component.
        belief.market_prob             = Some(market_prob);
        belief.market_confidence       = market_precision;
        belief.market_fusion_log_odds  = market_precision * (market_lo - prior_lo);

        // Record in history (replaces nothing — treated as a new entry).
        if belief.update_history.len() >= MAX_HISTORY {
            belief.update_history.pop_front();
        }
        belief.update_history.push_back(EvidenceEntry {
            timestamp: std::time::Instant::now(),
            log_odds_applied: belief.market_fusion_log_odds,
            source: EvidenceSource::MarketPrice,
        });

        belief.recompute();

        debug!(
            market_id,
            market_prob,
            market_precision,
            market_fusion_log_odds = belief.market_fusion_log_odds,
            posterior = belief.posterior_prob,
            "bayesian_engine: market price fused"
        );
        Ok(())
    }

    /// Force-recompute the posterior for `market_id` from its stored log-odds
    /// components and return the result.
    ///
    /// The value is also cached in [`BeliefState::posterior_prob`].  In normal
    /// operation mutation methods (update_prior, ingest_evidence, fuse_market_price)
    /// keep `posterior_prob` current, so this call is mostly for safety/testing.
    ///
    /// # Errors
    /// Returns [`BayesianError::MarketNotFound`] if the market has no belief state.
    pub fn compute_posterior(&mut self, market_id: &str) -> Result<f64, BayesianError> {
        let belief = self
            .beliefs
            .get_mut(market_id)
            .ok_or_else(|| BayesianError::MarketNotFound(market_id.to_string()))?;
        Ok(belief.recompute())
    }

    /// Recompute posteriors for **all** tracked markets and return a snapshot
    /// of `(market_id, new_posterior)` pairs.
    ///
    /// This is a no-allocation-per-market path: it uses the accumulated
    /// `evidence_log_odds` and `market_fusion_log_odds` already stored in each
    /// [`BeliefState`] without adding new evidence.
    ///
    /// Call this after a batch of `ingest_evidence` calls (e.g. after
    /// processing a bulk news feed) to propagate all updates at once.
    pub fn batch_update_posteriors(&mut self) -> Vec<(String, f64)> {
        self.beliefs
            .values_mut()
            .map(|b| {
                let p = b.recompute();
                (b.market_id.clone(), p)
            })
            .collect()
    }

    /// `posterior_prob − market_prob` for `market_id`.
    ///
    /// Falls back to `posterior_prob − prior_prob` when no market price has
    /// been fused yet.
    ///
    /// * **Positive** → model is above market price → potential BUY signal.
    /// * **Negative** → model is below market price → potential SELL signal.
    ///
    /// # Errors
    /// Returns [`BayesianError::MarketNotFound`] if the market has no belief state.
    pub fn get_deviation_from_market(&self, market_id: &str) -> Result<f64, BayesianError> {
        let belief = self
            .beliefs
            .get(market_id)
            .ok_or_else(|| BayesianError::MarketNotFound(market_id.to_string()))?;
        let reference = belief.market_prob.unwrap_or(belief.prior_prob);
        Ok(belief.posterior_prob - reference)
    }

    /// Probability-space blend of the model posterior and the latest market
    /// price, weighted by `market_confidence`.
    ///
    /// Unlike `posterior_prob` (which fuses in log-odds space), this blends
    /// directly in probability space:
    ///
    /// ```text
    /// result = (1 − market_confidence) × model_prob + market_confidence × market_prob
    /// ```
    ///
    /// where `model_prob = sigmoid(log_odds(prior) + evidence_log_odds)` is
    /// the non-market-fused posterior.  Useful when a strategy layer wants a
    /// linear interpolation between model and market rather than a log-odds
    /// fusion.
    ///
    /// Falls back to `posterior_prob` when no market price is available.
    ///
    /// # Errors
    /// Returns [`BayesianError::MarketNotFound`] if the market has no belief state.
    pub fn get_confidence_weighted_posterior(
        &self,
        market_id: &str,
    ) -> Result<f64, BayesianError> {
        let belief = self
            .beliefs
            .get(market_id)
            .ok_or_else(|| BayesianError::MarketNotFound(market_id.to_string()))?;

        let Some(mp) = belief.market_prob else {
            return Ok(belief.posterior_prob);
        };

        // Model-only posterior: prior + non-market evidence, no market fusion.
        let model_lo   = prob_to_log_odds(belief.prior_prob) + belief.evidence_log_odds;
        let model_prob = log_odds_to_prob(model_lo);

        let w = belief.market_confidence;
        Ok(w * mp + (1.0 - w) * model_prob)
    }

    /// Compute a log-odds signal strength and Kelly-optimal position size.
    ///
    /// Requires that [`fuse_market_price`](Self::fuse_market_price) has already
    /// been called for `market_id` so a reference market price is available.
    ///
    /// # Algorithm
    ///
    /// ```text
    /// p  = posterior_prob (clamped to [0.001, 0.999])
    /// m  = market_price   (clamped to [0.001, 0.999])
    /// c  = sqrt(history_len / MAX_HISTORY)   ← evidence density, not market_confidence
    ///
    /// edge            = logit(p) − logit(m)
    /// signal_strength = edge × c
    ///
    /// YES (p > m):  kelly_raw = (p − m) / (1 − m)
    /// NO  (p < m):  kelly_raw = −(m − p) / m       ← correct NO sizing
    /// kelly_fraction = clamp(kelly_raw, −1, 1) × c
    /// ```
    ///
    /// # Example (64 history entries → c = 0.80)
    ///
    /// ```text
    /// posterior = 0.65, market = 0.55, c = 0.80
    ///
    /// edge            ≈ 0.6190 − 0.2007 = 0.4183
    /// signal_strength ≈ 0.4183 × 0.80   = 0.3347
    ///
    /// kelly_raw (YES) = (0.65 − 0.55) / (1 − 0.55) = 0.2222
    /// kelly_fraction  = 0.2222 × 0.80 ≈ 0.1778
    /// ```
    ///
    /// # Errors
    ///
    /// * [`BayesianError::MarketNotFound`] — market has no belief state.
    /// * [`BayesianError::MissingMarketPrice`] — `fuse_market_price` has not
    ///   been called for this market yet.
    pub fn get_signal_and_kelly(&self, market_id: &str) -> Result<SignalResult, BayesianError> {
        let belief = self
            .beliefs
            .get(market_id)
            .ok_or_else(|| BayesianError::MarketNotFound(market_id.to_string()))?;

        let raw_market = belief
            .market_prob
            .ok_or_else(|| BayesianError::MissingMarketPrice(market_id.to_string()))?;

        // Clamp both ends to keep logit and payout-odds finite.
        let p = clamp_probability(belief.posterior_prob);
        let m = clamp_probability(raw_market);

        // Model confidence = evidence density, sqrt-scaled for diminishing returns.
        //
        // `market_confidence` (set by `fuse_market_price`) measures trust *in the
        // market price*, not in the model's independent signal.  Using it here
        // creates an inverse relationship: high market trust → small independent
        // edge → confidence paradoxically peaks when the model defers to the market.
        //
        // Evidence density counts all history entries (evidence + market price
        // observations) as a proxy for total information processed.  sqrt scaling
        // gives half-confidence at 25 entries and full confidence at MAX_HISTORY.
        let n = belief.update_history.len();
        let model_confidence = ((n as f64) / (MAX_HISTORY as f64)).sqrt().clamp(0.0, 1.0);

        // ── Step 1: log-odds edge ──────────────────────────────────────────────
        let edge = prob_to_log_odds(p) - prob_to_log_odds(m);
        let signal_strength = edge * model_confidence;

        // ── Step 2: Kelly fraction ─────────────────────────────────────────────
        let kelly_raw     = compute_kelly(p, m);
        let kelly_clamped = kelly_raw.clamp(-1.0, 1.0);
        let adjusted_kelly = kelly_clamped * model_confidence;

        debug!(
            market_id,
            posterior  = p,
            market     = m,
            confidence = model_confidence,
            signal_strength,
            kelly_fraction = adjusted_kelly,
            "bayesian_engine: signal_and_kelly computed"
        );

        Ok(SignalResult {
            signal_strength,
            kelly_fraction: adjusted_kelly,
        })
    }

    /// Reset accumulated evidence for a market, optionally keeping the cached
    /// posterior and history.
    ///
    /// After this call `evidence_log_odds` and `market_fusion_log_odds` are
    /// zeroed, so the next `compute_posterior` will return a value equal to
    /// `prior_prob`.  This is useful when a market resolves and you want to
    /// start belief tracking fresh.
    ///
    /// # Errors
    /// Returns [`BayesianError::MarketNotFound`] if the market has no belief state.
    pub fn reset_evidence(&mut self, market_id: &str) -> Result<(), BayesianError> {
        let belief = self
            .beliefs
            .get_mut(market_id)
            .ok_or_else(|| BayesianError::MarketNotFound(market_id.to_string()))?;

        belief.evidence_log_odds      = 0.0;
        belief.market_fusion_log_odds = 0.0;
        belief.market_prob            = None;
        belief.market_confidence      = 0.0;
        belief.recompute();

        debug!(market_id, "bayesian_engine: evidence reset");
        Ok(())
    }

    /// Apply graph-propagation influence to all markets that have upstream
    /// neighbors in `graph`.
    ///
    /// For each market tracked by this engine:
    /// 1. Look up its upstream influencers in the graph.
    /// 2. For each influencer: compute the *current* delta between that
    ///    influencer's model posterior and its market price (i.e. how much the
    ///    Bayesian model disagrees with the market for the upstream node).
    /// 3. Scale by edge effective weight and upstream market confidence.
    /// 4. Convert the probability delta to log-odds and add to
    ///    `evidence_log_odds` via [`ingest_evidence`](Self::ingest_evidence).
    ///
    /// This is a convenience method for the non-async use-case (unit tests,
    /// batch back-tests).  In the async event-driven path the same logic runs
    /// inside the `BayesianModel::run` loop when a `GraphUpdate` arrives.
    ///
    /// Returns a list of `(market_id, new_posterior)` for every market updated.
    pub fn apply_graph_influence(
        &mut self,
        graph: &MarketGraphEngine,
    ) -> Vec<(String, f64)> {
        // Collect influence contributions first (immutable read of self + graph),
        // then apply them (mutable write of self) to avoid borrow conflicts.
        let market_ids: Vec<String> = self.beliefs.keys().cloned().collect();

        // (downstream_id, log_odds_delta)
        let mut contributions: Vec<(String, f64)> = Vec::new();

        for downstream_id in &market_ids {
            let Ok(influencers) = graph.get_influencing_nodes(downstream_id) else {
                continue;
            };

            for (upstream_id, eff_weight) in influencers {
                // Need upstream's posterior and market_confidence from our beliefs.
                let Some(upstream_belief) = self.beliefs.get(&upstream_id) else {
                    continue;
                };
                let upstream_posterior = upstream_belief.posterior_prob;
                let upstream_conf      = upstream_belief.market_confidence;

                // delta = how much the upstream's model posterior exceeds the
                // market price for that node.
                let upstream_market = upstream_belief.market_prob.unwrap_or(upstream_belief.prior_prob);
                let delta = upstream_posterior - upstream_market;

                if delta.abs() < 1e-6 {
                    continue;
                }

                // influence = edge_weight * delta * upstream_confidence
                let influence = eff_weight * delta * upstream_conf.max(0.1);

                // Convert to log-odds using upstream's market price as reference.
                let ref_prob = upstream_market.clamp(0.01, 0.99);
                let lo_delta = delta_prob_to_log_odds(influence, ref_prob);

                contributions.push((downstream_id.clone(), lo_delta));
            }
        }

        // Apply contributions with a fixed strength (graph is indirect evidence).
        let mut updated = Vec::new();
        for (downstream_id, lo_delta) in contributions {
            if let Ok(()) = self.ingest_evidence(
                &downstream_id,
                lo_delta,
                EvidenceSource::GraphPropagation,
                0.5, // half-strength: graph influence is indirect
            ) {
                if let Ok(p) = self.compute_posterior(&downstream_id) {
                    updated.push((downstream_id, p));
                }
            }
        }
        updated
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;

    const EPS: f64 = 1e-4;

    fn engine() -> BayesianEngine {
        BayesianEngine::new()
    }

    // ── 1. Prior update and posterior computation ─────────────────────────────

    #[test]
    fn test_prior_update_and_posterior_computation() {
        let mut e = engine();
        e.ensure_belief("trump_win", 0.5);

        // With no evidence, posterior == prior.
        let p = e.compute_posterior("trump_win").unwrap();
        assert_abs_diff_eq!(p, 0.5, epsilon = EPS);

        // Update prior to 0.7.
        e.update_prior("trump_win", 0.7).unwrap();
        let p = e.compute_posterior("trump_win").unwrap();
        assert_abs_diff_eq!(p, 0.7, epsilon = EPS);
    }

    // ── 2. Log-odds fusion correctness ────────────────────────────────────────

    #[test]
    fn test_log_odds_fusion_correctness() {
        let mut e = engine();
        e.ensure_belief("market_a", 0.5);

        // At prior 0.5 and no fusion, adding 1.0 log-odds → sigmoid(1.0) ≈ 0.7311
        e.ingest_evidence("market_a", 1.0, EvidenceSource::PollAggregate, 1.0)
            .unwrap();
        let p = e.compute_posterior("market_a").unwrap();

        // sigmoid(log_odds(0.5) + 1.0) = sigmoid(0 + 1.0) = sigmoid(1.0) ≈ 0.7311
        let expected = log_odds_to_prob(1.0);
        assert_abs_diff_eq!(p, expected, epsilon = EPS);
        assert!(p > 0.5, "positive evidence must increase posterior");
    }

    // ── 3. Negative evidence reduces posterior ────────────────────────────────

    #[test]
    fn test_negative_evidence_reduces_posterior() {
        let mut e = engine();
        e.ensure_belief("market_b", 0.6);

        e.ingest_evidence("market_b", -1.5, EvidenceSource::MacroIndicator, 0.8)
            .unwrap();
        let p = e.compute_posterior("market_b").unwrap();
        assert!(p < 0.6, "negative evidence must decrease posterior");
    }

    // ── 4. Market price fusion / confidence weighting ─────────────────────────

    #[test]
    fn test_market_price_fusion_full_precision() {
        let mut e = engine();
        e.ensure_belief("btc_100k", 0.5);

        // At precision = 1.0, the posterior should be pulled fully to market price.
        // Formula: posterior_lo = prior_lo + 0 + 1.0 * (market_lo - prior_lo) = market_lo
        // ⟹ posterior = market_prob
        e.fuse_market_price("btc_100k", 0.7, 1.0).unwrap();
        let p = e.compute_posterior("btc_100k").unwrap();
        assert_abs_diff_eq!(p, 0.7, epsilon = EPS);
    }

    #[test]
    fn test_market_price_fusion_zero_precision() {
        let mut e = engine();
        e.ensure_belief("btc_100k", 0.5);

        // At precision = 0.0, the market price has no effect.
        e.fuse_market_price("btc_100k", 0.9, 0.0).unwrap();
        let p = e.compute_posterior("btc_100k").unwrap();
        assert_abs_diff_eq!(p, 0.5, epsilon = EPS); // prior unchanged
    }

    #[test]
    fn test_market_price_fusion_partial_precision() {
        let mut e = engine();
        e.ensure_belief("eth_5k", 0.5);

        // At precision = 0.5, market price is blended.
        e.fuse_market_price("eth_5k", 0.8, 0.5).unwrap();
        let p = e.compute_posterior("eth_5k").unwrap();
        // posterior is between prior (0.5) and market price (0.8)
        assert!(p > 0.5 && p < 0.8, "partial precision should blend: got {p}");
    }

    // ── 5. Evidence ingestion from multiple sources ───────────────────────────

    #[test]
    fn test_evidence_from_multiple_sources_accumulates() {
        let mut e = engine();
        e.ensure_belief("gop_senate", 0.5);

        // Each source adds independent positive evidence.
        e.ingest_evidence("gop_senate", 0.5, EvidenceSource::PollAggregate, 1.0).unwrap();
        let p1 = e.compute_posterior("gop_senate").unwrap();

        e.ingest_evidence("gop_senate", 0.3, EvidenceSource::NewsSentiment, 0.8).unwrap();
        let p2 = e.compute_posterior("gop_senate").unwrap();

        e.ingest_evidence("gop_senate", 0.2, EvidenceSource::GraphPropagation, 0.5).unwrap();
        let p3 = e.compute_posterior("gop_senate").unwrap();

        // Each addition of positive evidence should further increase posterior.
        assert!(p2 > p1, "second evidence source should further increase posterior");
        assert!(p3 > p2, "third evidence source should further increase posterior");
        // History should record all three entries.
        assert_eq!(
            e.get_belief("gop_senate").unwrap().update_history.len(),
            3
        );
    }

    // ── 6. Deviation calculation from market price ────────────────────────────

    #[test]
    fn test_deviation_from_market() {
        let mut e = engine();
        e.ensure_belief("trump_win", 0.5);
        e.fuse_market_price("trump_win", 0.54, 0.7).unwrap();
        // Add positive evidence that pushes the model above market.
        e.ingest_evidence("trump_win", 1.0, EvidenceSource::PollAggregate, 1.0).unwrap();

        let p = e.compute_posterior("trump_win").unwrap();
        let dev = e.get_deviation_from_market("trump_win").unwrap();

        // deviation = posterior - market_prob
        assert_abs_diff_eq!(dev, p - 0.54, epsilon = EPS);
    }

    #[test]
    fn test_deviation_falls_back_to_prior_when_no_market_price() {
        let mut e = engine();
        e.ensure_belief("fed_cut", 0.4);
        e.ingest_evidence("fed_cut", 0.8, EvidenceSource::MacroIndicator, 1.0).unwrap();

        let p = e.compute_posterior("fed_cut").unwrap();
        let dev = e.get_deviation_from_market("fed_cut").unwrap();

        // no market price fused → deviation = posterior - prior
        assert_abs_diff_eq!(dev, p - 0.4, epsilon = EPS);
    }

    // ── 7. Batch posterior update ─────────────────────────────────────────────

    #[test]
    fn test_batch_update_posteriors() {
        let mut e = engine();
        for (id, prob) in [("a", 0.3), ("b", 0.5), ("c", 0.7)] {
            e.ensure_belief(id, prob);
            e.ingest_evidence(id, 0.5, EvidenceSource::PollAggregate, 1.0).unwrap();
        }

        let results = e.batch_update_posteriors();
        assert_eq!(results.len(), 3);

        // Every returned posterior should be above its prior (positive evidence).
        for (id, p) in &results {
            let prior = e.get_belief(id).unwrap().prior_prob;
            assert!(
                *p > prior,
                "market {id}: posterior {p} should exceed prior {prior}"
            );
        }
    }

    // ── 8. Market not found errors ────────────────────────────────────────────

    #[test]
    fn test_market_not_found_errors() {
        let mut e = engine();

        assert!(matches!(
            e.update_prior("ghost", 0.5),
            Err(BayesianError::MarketNotFound(_))
        ));
        assert!(matches!(
            e.ingest_evidence("ghost", 1.0, EvidenceSource::ManualOverride, 1.0),
            Err(BayesianError::MarketNotFound(_))
        ));
        assert!(matches!(
            e.fuse_market_price("ghost", 0.5, 0.8),
            Err(BayesianError::MarketNotFound(_))
        ));
        assert!(matches!(
            e.compute_posterior("ghost"),
            Err(BayesianError::MarketNotFound(_))
        ));
        assert!(matches!(
            e.get_deviation_from_market("ghost"),
            Err(BayesianError::MarketNotFound(_))
        ));
    }

    // ── 9. Invalid inputs rejected ────────────────────────────────────────────

    #[test]
    fn test_invalid_inputs_rejected() {
        let mut e = engine();
        e.ensure_belief("m", 0.5);

        assert!(matches!(
            e.update_prior("m", 1.5),
            Err(BayesianError::InvalidProbability(_))
        ));
        assert!(matches!(
            e.fuse_market_price("m", -0.1, 0.5),
            Err(BayesianError::InvalidProbability(_))
        ));
        assert!(matches!(
            e.fuse_market_price("m", 0.5, 1.5),
            Err(BayesianError::InvalidPrecision(_))
        ));
    }

    // ── 10. confidence_weighted_posterior ────────────────────────────────────

    #[test]
    fn test_confidence_weighted_posterior_blends_in_prob_space() {
        let mut e = engine();
        e.ensure_belief("m", 0.3);
        e.fuse_market_price("m", 0.7, 0.5).unwrap();

        let cwp = e.get_confidence_weighted_posterior("m").unwrap();
        // Should lie strictly between model and market price.
        // model_prob ≈ prior (no other evidence), market = 0.7
        let model_lo   = prob_to_log_odds(0.3); // no extra evidence
        let model_prob = log_odds_to_prob(model_lo);
        let expected   = 0.5 * 0.7 + 0.5 * model_prob;
        assert_abs_diff_eq!(cwp, expected, epsilon = EPS);
    }

    // ── 11. Market price is idempotent (replace, not accumulate) ─────────────

    #[test]
    fn test_fuse_market_price_replaces_not_accumulates() {
        let mut e = engine();
        e.ensure_belief("m", 0.5);

        // Fuse the same market price twice — result must equal fusing once.
        e.fuse_market_price("m", 0.8, 0.9).unwrap();
        let p_once = e.compute_posterior("m").unwrap();

        e.fuse_market_price("m", 0.8, 0.9).unwrap(); // second call, same args
        let p_twice = e.compute_posterior("m").unwrap();

        assert_abs_diff_eq!(p_once, p_twice, epsilon = 1e-12);
    }

    // ── 12. get_signal_and_kelly ──────────────────────────────────────────────

    #[test]
    fn test_signal_and_kelly_yes_direction() {
        // Build a controlled state: prior=0.55, market=0.55, then inject evidence
        // to lift posterior to exactly 0.65.  p=0.65 > m=0.55 → YES position.
        let mut e = engine();
        e.ensure_belief("spec", 0.55);
        e.fuse_market_price("spec", 0.55, 0.80).unwrap(); // history entry #1

        // At prior=market=0.55 the posterior is 0.55.  Add exactly enough log-odds
        // to reach 0.65: Δ = logit(0.65) − logit(0.55).
        let needed = prob_to_log_odds(0.65) - prob_to_log_odds(0.55);
        e.ingest_evidence("spec", needed, EvidenceSource::ManualOverride, 1.0).unwrap(); // #2

        let posterior = e.compute_posterior("spec").unwrap();
        assert_abs_diff_eq!(posterior, 0.65, epsilon = 1e-4);

        let res = e.get_signal_and_kelly("spec").unwrap();

        // model_confidence = sqrt(history_len / MAX_HISTORY) = sqrt(2/100)
        let n   = e.get_belief("spec").unwrap().update_history.len();
        let c   = ((n as f64) / (MAX_HISTORY as f64)).sqrt();

        // signal_strength = (logit(0.65) − logit(0.55)) × c
        let expected_signal = (prob_to_log_odds(0.65) - prob_to_log_odds(0.55)) * c;
        assert_abs_diff_eq!(res.signal_strength, expected_signal, epsilon = 1e-4);

        // YES Kelly = (p − m) / (1 − m) = (0.65 − 0.55) / (1 − 0.55) = 0.2222
        let expected_kelly = (0.65_f64 - 0.55_f64) / (1.0_f64 - 0.55_f64) * c;
        assert_abs_diff_eq!(res.kelly_fraction, expected_kelly, epsilon = 1e-4);

        assert!(res.signal_strength > 0.0, "positive edge → positive signal");
        assert!(res.kelly_fraction  > 0.0, "positive edge → positive Kelly (YES)");
    }

    #[test]
    fn test_signal_negative_when_posterior_below_market() {
        let mut e = engine();
        e.ensure_belief("m", 0.80); // prior = 0.80
        e.fuse_market_price("m", 0.80, 0.70).unwrap();
        // Push posterior down to ~0.40 with strong negative evidence.
        e.ingest_evidence("m", -3.0, EvidenceSource::MacroIndicator, 1.0).unwrap();
        e.compute_posterior("m").unwrap();

        let res = e.get_signal_and_kelly("m").unwrap();
        assert!(res.signal_strength < 0.0, "posterior < market → negative signal");
        assert!(res.kelly_fraction  < 0.0, "should recommend short / NO position");
    }

    #[test]
    fn test_no_kelly_magnitude_is_correct() {
        // p=0.40, market=0.60 → model is below market → NO/short position.
        //
        // Correct NO Kelly (before confidence scaling):
        //   kelly_no = (m − p) / m = (0.60 − 0.40) / 0.60 ≈ 0.3333
        //
        // The old (incorrect) YES-formula negative gives:
        //   (p − m) / (1 − m) = (0.40 − 0.60) / (1 − 0.60) = −0.50
        //
        // These differ significantly; the test verifies the correct 0.3333 magnitude.
        let mut e = engine();
        e.ensure_belief("m", 0.40);
        // Precision 0.0 → market price recorded but posterior stays at prior (0.40).
        e.fuse_market_price("m", 0.60, 0.0).unwrap();
        e.compute_posterior("m").unwrap();

        let belief = e.get_belief("m").unwrap();
        assert_abs_diff_eq!(belief.posterior_prob, 0.40, epsilon = EPS);

        let n = belief.update_history.len();
        let c = ((n as f64) / (MAX_HISTORY as f64)).sqrt().clamp(0.0, 1.0);

        let res = e.get_signal_and_kelly("m").unwrap();

        // Correct NO Kelly magnitude = (m − p) / m = 0.3333.
        let p = clamp_probability(0.40_f64);
        let m = clamp_probability(0.60_f64);
        let expected_raw  = -(m - p) / m;                      // ≈ −0.3333
        let expected_kelly = expected_raw.clamp(-1.0, 1.0) * c;

        assert_abs_diff_eq!(res.kelly_fraction, expected_kelly, epsilon = EPS);
        assert!(res.kelly_fraction < 0.0, "model below market → short direction");

        // Verify the magnitude matches (m−p)/m, not the wrong (m−p)/(1−m).
        let magnitude = res.kelly_fraction.abs() / c;
        assert_abs_diff_eq!(magnitude, (m - p) / m, epsilon = EPS);          // 0.3333 ✓
        let wrong_magnitude = (m - p) / (1.0 - m);                           // 0.5000
        assert!((magnitude - wrong_magnitude).abs() > 0.1,
            "Kelly magnitude {magnitude:.4} should not match wrong YES-formula {wrong_magnitude:.4}");
    }

    #[test]
    fn test_signal_and_kelly_missing_market_price_error() {
        let mut e = engine();
        e.ensure_belief("m", 0.5);
        // No fuse_market_price call → MissingMarketPrice.
        assert!(matches!(
            e.get_signal_and_kelly("m"),
            Err(BayesianError::MissingMarketPrice(_))
        ));
    }

    #[test]
    fn test_signal_and_kelly_market_not_found_error() {
        let e = engine();
        assert!(matches!(
            e.get_signal_and_kelly("ghost"),
            Err(BayesianError::MarketNotFound(_))
        ));
    }

    #[test]
    fn test_signal_at_zero_when_posterior_equals_market() {
        // When model exactly agrees with market, signal and raw kelly should both
        // be 0 (before confidence scaling).
        let mut e = engine();
        e.ensure_belief("m", 0.60);
        // precision = 1.0 → posterior fully adopts market price = 0.60
        e.fuse_market_price("m", 0.60, 1.0).unwrap();
        e.compute_posterior("m").unwrap();

        let res = e.get_signal_and_kelly("m").unwrap();
        // edge = logit(0.60) - logit(0.60) = 0
        assert_abs_diff_eq!(res.signal_strength, 0.0, epsilon = 1e-6);
        // kelly_raw = (b*p - q)/b where p=0.60, b=(1/0.60)-1=0.6667
        // = (0.6667*0.60 - 0.40)/0.6667 = (0.4000 - 0.40)/0.6667 = 0
        assert_abs_diff_eq!(res.kelly_fraction, 0.0, epsilon = 1e-5);
    }

    #[test]
    fn test_kelly_clamped_to_unit_interval() {
        // Extreme edge: posterior very high, market very low → huge raw Kelly.
        let mut e = engine();
        e.ensure_belief("m", 0.99);
        e.fuse_market_price("m", 0.01, 0.50).unwrap();
        // Restore posterior toward 0.99 by injecting overwhelming evidence.
        e.ingest_evidence("m", 20.0, EvidenceSource::ManualOverride, 1.0).unwrap();
        e.compute_posterior("m").unwrap();

        let res = e.get_signal_and_kelly("m").unwrap();
        assert!(
            res.kelly_fraction >= -1.0 && res.kelly_fraction <= 1.0,
            "Kelly fraction must be in [−1, 1]; got {}",
            res.kelly_fraction
        );
    }

    // ── 13. reset_evidence zeroes log-odds ────────────────────────────────────

    #[test]
    fn test_reset_evidence_zeroes_log_odds() {
        let mut e = engine();
        e.ensure_belief("m", 0.4);
        e.ingest_evidence("m", 2.0, EvidenceSource::PollAggregate, 1.0).unwrap();
        e.fuse_market_price("m", 0.6, 0.8).unwrap();

        // Before reset: posterior is well above prior.
        let p_before = e.compute_posterior("m").unwrap();
        assert!(p_before > 0.4);

        e.reset_evidence("m").unwrap();

        // After reset: posterior equals prior.
        let p_after = e.compute_posterior("m").unwrap();
        assert_abs_diff_eq!(p_after, 0.4, epsilon = EPS);
    }
}
