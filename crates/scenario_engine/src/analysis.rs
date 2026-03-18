// crates/scenario_engine/src/analysis.rs
//
// Expectation analysis and mispricing detection.
//
// ## Computed quantities
//
// Marginal probability  P(A) = #{scenarios where A=true} / N
//
// Joint probability  P(A ∧ B) = #{scenarios where A=true AND B=true} / N
//   Stored only for the canonical orientation where A < B lexicographically.
//   Halves storage vs. storing both (A,B) and (B,A).
//
// Independence deviation  |P(A∧B) − P(A)·P(B)|
//   Used to rank and filter joint pairs in the output event.
//
// Mispricing  expected_prob − market_prob
//   If |mispricing| >= threshold, a `PendingSignal` is produced.

use std::collections::HashMap;

use crate::types::{MarketId, PendingSignal, ScenarioBatch, ScenarioExpectation};

/// Derive marginal and joint probability estimates from a scenario batch.
///
/// Joint probabilities are stored only for the canonical orientation
/// where `market_a < market_b` lexicographically, reducing memory by half.
/// Use `joint_prob_lookup` to query either orientation.
pub fn compute_expectations(batch: &ScenarioBatch) -> ScenarioExpectation {
    if batch.scenarios.is_empty() {
        return ScenarioExpectation::default();
    }

    // Collect the union of all market IDs, sorted for deterministic pair order.
    let market_ids: Vec<MarketId> = {
        let mut seen = std::collections::HashSet::new();
        for s in &batch.scenarios {
            seen.extend(s.market_outcomes.keys().cloned());
        }
        let mut ids: Vec<_> = seen.into_iter().collect();
        ids.sort_unstable();
        ids
    };

    let n = batch.scenarios.len() as f64;

    // ── Marginal probabilities ───────────────────────────────────────────────
    let expected_probabilities: HashMap<MarketId, f64> = market_ids
        .iter()
        .map(|id| {
            let count = batch
                .scenarios
                .iter()
                .filter(|s| *s.market_outcomes.get(id).unwrap_or(&false))
                .count();
            (id.clone(), count as f64 / n)
        })
        .collect();

    // ── Joint probabilities — canonical (A < B) pairs only ──────────────────
    let mut joint_probabilities = HashMap::new();
    for i in 0..market_ids.len() {
        for j in (i + 1)..market_ids.len() {
            let a = &market_ids[i]; // a < b guaranteed by sorted order + i < j
            let b = &market_ids[j];
            let count = batch
                .scenarios
                .iter()
                .filter(|s| {
                    *s.market_outcomes.get(a).unwrap_or(&false)
                        && *s.market_outcomes.get(b).unwrap_or(&false)
                })
                .count();
            // Store only the canonical (A, B) orientation.
            joint_probabilities.insert((a.clone(), b.clone()), count as f64 / n);
        }
    }

    ScenarioExpectation {
        expected_probabilities,
        joint_probabilities,
    }
}

/// Look up P(A ∧ B) from canonical joint-probability storage.
///
/// Returns `None` if the pair is not present (either not computed or markets
/// are unknown).  Normalises key order so callers need not know which
/// orientation is stored.
pub fn joint_prob_lookup(
    expectation: &ScenarioExpectation,
    a: &str,
    b: &str,
) -> Option<f64> {
    if a <= b {
        expectation
            .joint_probabilities
            .get(&(a.to_owned(), b.to_owned()))
            .copied()
    } else {
        expectation
            .joint_probabilities
            .get(&(b.to_owned(), a.to_owned()))
            .copied()
    }
}

/// Select the top `limit` joint-probability pairs by deviation from
/// independence |P(A∧B) − P(A)·P(B)|, encoded as `"A|B"` string keys.
///
/// Passing `limit = 0` returns all pairs.
pub fn top_joint_pairs(
    expectation: &ScenarioExpectation,
    limit: usize,
) -> HashMap<String, f64> {
    // All entries are already canonical (A < B), so no orientation filter needed.
    let mut ranked: Vec<(String, f64, f64)> = expectation
        .joint_probabilities
        .iter()
        .map(|((a, b), &joint)| {
            let pa = expectation
                .expected_probabilities
                .get(a)
                .copied()
                .unwrap_or(0.0);
            let pb = expectation
                .expected_probabilities
                .get(b)
                .copied()
                .unwrap_or(0.0);
            let deviation = (joint - pa * pb).abs();
            (format!("{a}|{b}"), joint, deviation)
        })
        .collect();

    // Descending by deviation from independence.
    ranked.sort_unstable_by(|x, y| {
        y.2.partial_cmp(&x.2).unwrap_or(std::cmp::Ordering::Equal)
    });

    let take = if limit == 0 { ranked.len() } else { limit };
    ranked
        .into_iter()
        .take(take)
        .map(|(key, joint, _)| (key, joint))
        .collect()
}

/// Identify markets where the scenario-expected probability deviates from the
/// raw market price by at least `threshold`.
///
/// `confidence_map` — confidence per market, defaulting to `0.5` if absent.
pub fn detect_mispricing(
    expectation: &ScenarioExpectation,
    market_prices: &HashMap<MarketId, f64>,
    confidence_map: &HashMap<MarketId, f64>,
    threshold: f64,
) -> Vec<PendingSignal> {
    expectation
        .expected_probabilities
        .iter()
        .filter_map(|(market_id, &expected)| {
            let &market_prob = market_prices.get(market_id)?;
            let mispricing = expected - market_prob;
            if mispricing.abs() < threshold {
                return None;
            }
            let confidence = confidence_map.get(market_id).copied().unwrap_or(0.5);
            Some(PendingSignal {
                market_id: market_id.clone(),
                expected_probability: expected,
                market_probability: market_prob,
                mispricing,
                confidence,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sampler::sample_scenarios;

    fn beliefs(pairs: &[(&str, f64)]) -> HashMap<MarketId, f64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn expectations_converge_to_probabilities() {
        // With 10k samples, expected freq should be within 3% of the true prob.
        let b = beliefs(&[("A", 0.7), ("B", 0.3), ("C", 0.5)]);
        let batch = sample_scenarios(&b, &[], 10_000);
        let exp = compute_expectations(&batch);

        for (id, &true_prob) in &b {
            let est = exp.expected_probabilities[id];
            assert!(
                (est - true_prob).abs() < 0.03,
                "market {id}: expected ~{true_prob:.2}, got {est:.3}"
            );
        }
    }

    #[test]
    fn joint_probability_independent_markets() {
        // P(A∧B) ≈ P(A)·P(B) for independent markets.
        let b = beliefs(&[("A", 0.6), ("B", 0.4)]);
        let batch = sample_scenarios(&b, &[], 10_000);
        let exp = compute_expectations(&batch);

        let pa = exp.expected_probabilities["A"];
        let pb = exp.expected_probabilities["B"];
        // "A" < "B" so canonical pair is (A, B).
        let pab = exp.joint_probabilities[&("A".to_string(), "B".to_string())];

        assert!(
            (pab - pa * pb).abs() < 0.03,
            "P(A∧B)={pab:.3} ≠ P(A)·P(B)={:.3}",
            pa * pb
        );
    }

    #[test]
    fn joint_prob_lookup_normalises_key_order() {
        let b = beliefs(&[("A", 0.5), ("B", 0.5)]);
        let batch = sample_scenarios(&b, &[], 1_000);
        let exp = compute_expectations(&batch);

        // Both orientations should return the same value.
        let ab = joint_prob_lookup(&exp, "A", "B");
        let ba = joint_prob_lookup(&exp, "B", "A");
        assert!(ab.is_some());
        assert_eq!(ab, ba, "joint_prob_lookup must be symmetric");
    }

    #[test]
    fn top_joint_pairs_respects_limit() {
        let b = beliefs(&[("A", 0.5), ("B", 0.5), ("C", 0.5), ("D", 0.5)]);
        let batch = sample_scenarios(&b, &[], 500);
        let exp = compute_expectations(&batch);

        let top2 = top_joint_pairs(&exp, 2);
        assert_eq!(top2.len(), 2);
    }

    #[test]
    fn top_joint_pairs_zero_limit_returns_all() {
        let b = beliefs(&[("A", 0.5), ("B", 0.5), ("C", 0.5)]);
        let batch = sample_scenarios(&b, &[], 500);
        let exp = compute_expectations(&batch);

        // 3 markets → 3 canonical pairs.
        let all = top_joint_pairs(&exp, 0);
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn mispricing_detection_emits_signal_above_threshold() {
        let mut expected_probs = HashMap::new();
        expected_probs.insert("MKT".to_string(), 0.80);

        let mut market_prices = HashMap::new();
        market_prices.insert("MKT".to_string(), 0.55);

        let exp = ScenarioExpectation {
            expected_probabilities: expected_probs,
            joint_probabilities: HashMap::new(),
        };
        let signals = detect_mispricing(&exp, &market_prices, &HashMap::new(), 0.10);

        assert_eq!(signals.len(), 1);
        let sig = &signals[0];
        assert_eq!(sig.market_id, "MKT");
        assert!((sig.mispricing - 0.25).abs() < 1e-9);
    }

    #[test]
    fn mispricing_below_threshold_produces_no_signal() {
        let mut expected_probs = HashMap::new();
        expected_probs.insert("MKT".to_string(), 0.52);

        let mut market_prices = HashMap::new();
        market_prices.insert("MKT".to_string(), 0.50);

        let exp = ScenarioExpectation {
            expected_probabilities: expected_probs,
            joint_probabilities: HashMap::new(),
        };
        let signals = detect_mispricing(&exp, &market_prices, &HashMap::new(), 0.10);
        assert!(signals.is_empty());
    }

    #[test]
    fn no_market_price_produces_no_signal() {
        let mut expected_probs = HashMap::new();
        expected_probs.insert("UNKNOWN".to_string(), 0.99);

        let exp = ScenarioExpectation {
            expected_probabilities: expected_probs,
            joint_probabilities: HashMap::new(),
        };
        let signals = detect_mispricing(&exp, &HashMap::new(), &HashMap::new(), 0.05);
        assert!(signals.is_empty());
    }
}
