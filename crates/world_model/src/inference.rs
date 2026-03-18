// crates/world_model/src/inference.rs
//
// Lightweight belief propagation for the World Model Engine.
//
// Algorithm
// ─────────
// 1. Compute Δ log-odds using the *caller-supplied* old probability vs new_prob.
//    The caller must capture old_prob from state BEFORE modifying the market
//    variable, so that the delta is non-zero when the probability changes.
// 2. BFS outward through `WorldDependency` edges.
// 3. At each edge AB with weight w and confidence c:
//        Δ_B = w × c × Δ_A × damping
// 4. Apply Δ_B to child's log-odds and convert back to probability.
// 5. Visited-set prevents cycles; hop limit prevents deep propagation.
//
// Numerical stability: all probabilities are clamped before log-odds
// conversion so that ln(0) and ln(∞) cannot occur.

use std::collections::{HashSet, VecDeque};

use chrono::Utc;

use crate::types::{InfluencePropagation, WorldInferenceResult, WorldState};

// ---------------------------------------------------------------------------
// Public math helpers
// ---------------------------------------------------------------------------

/// Convert probability to log-odds.  Clamps `p` to `(1e-9, 1 − 1e-9)` to
/// avoid ±∞.
#[inline]
pub fn prob_to_log_odds(p: f64) -> f64 {
    let p = p.clamp(1e-9, 1.0 - 1e-9);
    (p / (1.0 - p)).ln()
}

/// Convert log-odds back to probability.  Numerically stable via the
/// standard sigmoid.
#[inline]
pub fn log_odds_to_prob(lo: f64) -> f64 {
    // Clamp extreme log-odds to avoid underflow/overflow in exp().
    let lo = lo.clamp(-50.0, 50.0);
    1.0 / (1.0 + (-lo).exp())
}

// ---------------------------------------------------------------------------
// Propagation
// ---------------------------------------------------------------------------

/// Propagate a probability update for `source_market_id` through the world
/// model's dependency graph using BFS.
///
/// # IMPORTANT — caller responsibility
///
/// The caller **must** capture `old_prob` from `state` *before* writing
/// `new_prob` into the market variable.  If the caller writes `new_prob` to
/// `state.variables[source_market_id].current_probability` first and then
/// passes `new_prob` as both arguments, `delta_lo` will be zero and no
/// propagation will occur.
///
/// # Arguments
/// - `state`            — mutable world state; both `variables` and
///                        `dependencies` are accessed (read deps, write vars).
/// - `source_market_id` — the market whose probability changed.
/// - `old_prob`         — the previous probability for the source market,
///                        captured by the caller **before** updating state.
/// - `new_prob`         — the new probability for the source market.
/// - `result`           — accumulates updates and propagation log.
/// - `max_hops`         — maximum BFS depth.
/// - `damping`          — multiplicative attenuation per hop [0, 1].
/// - `threshold`        — minimum |Δ log-odds| to continue propagating.
pub fn propagate_update(
    state: &mut WorldState,
    source_market_id: &str,
    old_prob: f64,
    new_prob: f64,
    result: &mut WorldInferenceResult,
    max_hops: usize,
    damping: f64,
    threshold: f64,
) {
    let delta_lo = prob_to_log_odds(new_prob) - prob_to_log_odds(old_prob);

    // Record the source market as updated (even if delta is zero, the caller
    // has already written new_prob into state before calling us).
    result
        .updated_probabilities
        .insert(source_market_id.to_string(), new_prob);

    // No meaningful change — nothing to propagate.
    if delta_lo.abs() < threshold {
        return;
    }

    // Snapshot dependencies to avoid simultaneous mutable/immutable borrow
    // on WorldState while the BFS mutates variables.
    // For typical world-model sizes (tens to hundreds of edges) this clone
    // is cheap and avoids unsafe split-borrow gymnastics.
    let deps: Vec<(String, String, f64, f64)> = state
        .dependencies
        .iter()
        .map(|d| {
            (
                d.parent_market.clone(),
                d.child_market.clone(),
                d.weight,
                d.confidence,
            )
        })
        .collect();

    // BFS queue: (market_id, effective_delta_lo, hop_count)
    let mut queue: VecDeque<(String, f64, usize)> = VecDeque::new();
    queue.push_back((source_market_id.to_string(), delta_lo, 0));

    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(source_market_id.to_string());

    while let Some((parent_id, parent_delta, hop)) = queue.pop_front() {
        if hop >= max_hops {
            continue;
        }

        for (from, to, weight, confidence) in &deps {
            if from != &parent_id || visited.contains(to.as_str()) {
                continue;
            }

            // Apply edge weight, confidence, and per-hop damping.
            let child_delta = weight * confidence * parent_delta * damping;
            if child_delta.abs() < threshold {
                // Below propagation threshold — prune this branch.
                continue;
            }

            // Apply the delta to the child's current log-odds.
            if let Some(child_var) = state.variables.get_mut(to.as_str()) {
                let current_lo = prob_to_log_odds(child_var.current_probability);
                child_var.current_probability = log_odds_to_prob(current_lo + child_delta);
                child_var.last_update_timestamp = Utc::now();
                result
                    .updated_probabilities
                    .insert(to.clone(), child_var.current_probability);
            }

            result.influence_propagations.push(InfluencePropagation {
                from_market: parent_id.clone(),
                to_market: to.clone(),
                delta_log_odds: child_delta,
                weight: *weight,
            });

            visited.insert(to.clone());
            queue.push_back((to.clone(), child_delta, hop + 1));
        }
    }
}
