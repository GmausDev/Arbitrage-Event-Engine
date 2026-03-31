// crates/scenario_engine/src/sampler.rs
//
// Monte Carlo scenario sampler.
//
// ## Algorithm
//
// For each scenario, markets are sampled in a two-pass strategy:
//
// **Pass 1** — independent sample: every market is drawn from its base
// world-model probability.
//
// **Pass 2** — dependency adjustment: markets that have at least one parent
// dependency are re-drawn using an effective probability:
//
//   effective_prob(child) =
//       clamp(base_prob[child] + Σ weight_i × parent_outcome_pass1, 0, 1)
//
// Using pass-1 parent outcomes makes the result order-independent for
// single-hop dependency graphs.  Multi-hop chains are approximated by one
// round of adjustment; the Monte Carlo average converges correctly regardless.
//
// ## Performance
//
// A `child_deps` HashMap is pre-built once per batch to avoid an O(D) scan
// of the full dependency list for every market in every scenario.

use std::collections::HashMap;
use std::time::Instant;

use chrono::Utc;
use rand::Rng;

use crate::error::ScenarioEngineError;
use crate::types::{DependencyEdge, MarketId, Scenario, ScenarioBatch};

/// Generate `sample_size` independent Monte Carlo scenarios.
///
/// `beliefs` — world-model probability for each market in [0, 1].
/// `dependencies` — influence edges applied during sampling.
///
/// # Errors
///
/// Returns `ScenarioEngineError::EmptyBeliefState` if `beliefs` is empty.
/// Returns `ScenarioEngineError::InvalidSampleSize` if `sample_size` is 0.
/// Returns `ScenarioEngineError::InvalidProbability` if any belief probability is outside [0.0, 1.0].
pub fn sample_scenarios(
    beliefs: &HashMap<MarketId, f64>,
    dependencies: &[DependencyEdge],
    sample_size: usize,
) -> Result<ScenarioBatch, ScenarioEngineError> {
    if beliefs.is_empty() {
        return Err(ScenarioEngineError::EmptyBeliefState);
    }
    if sample_size == 0 {
        return Err(ScenarioEngineError::InvalidSampleSize(0));
    }
    for (_, &prob) in beliefs {
        if !(0.0..=1.0).contains(&prob) {
            return Err(ScenarioEngineError::InvalidProbability(prob));
        }
    }

    let start = Instant::now();
    // Capture once; all scenarios in this batch share the same timestamp.
    let batch_timestamp = Utc::now();

    // Stable iteration order for reproducible dependency adjustments.
    let market_ids: Vec<MarketId> = {
        let mut ids: Vec<_> = beliefs.keys().cloned().collect();
        ids.sort_unstable();
        ids
    };

    // Pre-build a child-indexed lookup so sample_one can find each market's
    // parents in O(1) instead of scanning the full dependency list.
    let child_deps: HashMap<String, Vec<(String, f64)>> = {
        let mut m: HashMap<String, Vec<(String, f64)>> = HashMap::new();
        for dep in dependencies {
            m.entry(dep.child.clone())
                .or_default()
                .push((dep.parent.clone(), dep.weight));
        }
        m
    };

    let weight = if sample_size > 0 {
        1.0 / sample_size as f64
    } else {
        0.0
    };

    let mut rng = rand::thread_rng();
    let mut scenarios = Vec::with_capacity(sample_size);

    for id in 0..sample_size {
        let market_outcomes = sample_one(beliefs, &child_deps, &market_ids, &mut rng);
        scenarios.push(Scenario {
            scenario_id: id as u64,
            market_outcomes,
            probability_weight: weight,
            timestamp: batch_timestamp,
        });
    }

    Ok(ScenarioBatch {
        scenarios,
        sample_size,
        generation_time_ms: start.elapsed().as_millis() as u64,
    })
}

/// Sample one scenario with two passes.
///
/// `child_deps` maps each child market to its parent list — pre-built by
/// `sample_scenarios` to avoid a linear scan of the full dependency list
/// for every market on every scenario.
fn sample_one(
    beliefs: &HashMap<MarketId, f64>,
    child_deps: &HashMap<String, Vec<(String, f64)>>,
    market_ids: &[MarketId],
    rng: &mut impl Rng,
) -> HashMap<MarketId, bool> {
    // ── Pass 1: independent samples ─────────────────────────────────────────
    let mut outcomes: HashMap<MarketId, bool> = market_ids
        .iter()
        .map(|id| {
            let prob = beliefs.get(id).copied().unwrap_or(0.5);
            (id.clone(), rng.gen::<f64>() < prob)
        })
        .collect();

    // ── Pass 2: dependency-adjusted re-samples ───────────────────────────────
    for market_id in market_ids {
        let Some(parents) = child_deps.get(market_id) else {
            continue; // no dependency parents — skip
        };

        // Use pass-1 parent outcomes so the result is ordering-independent.
        let adjustment: f64 = parents
            .iter()
            .map(|(parent, weight)| {
                let parent_true = outcomes.get(parent.as_str()).copied().unwrap_or(false);
                weight * if parent_true { 1.0 } else { 0.0 }
            })
            .sum();

        if adjustment.abs() < 1e-12 {
            continue; // no active parent influence — skip re-sample
        }

        let base_prob = beliefs.get(market_id).copied().unwrap_or(0.5);
        let effective_prob = (base_prob + adjustment).clamp(0.0, 1.0);
        outcomes.insert(market_id.clone(), rng.gen::<f64>() < effective_prob);
    }

    outcomes
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn beliefs(pairs: &[(&str, f64)]) -> HashMap<MarketId, f64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn outcomes_are_boolean() {
        let b = beliefs(&[("A", 0.6), ("B", 0.3)]);
        let batch = sample_scenarios(&b, &[], 100).unwrap();
        for scenario in &batch.scenarios {
            assert_eq!(scenario.market_outcomes.len(), 2, "missing market outcomes");
        }
    }

    #[test]
    fn empty_beliefs_returns_error() {
        let result = sample_scenarios(&HashMap::new(), &[], 500);
        assert!(result.is_err());
    }

    #[test]
    fn zero_sample_size_returns_error() {
        let b = beliefs(&[("A", 0.5)]);
        let result = sample_scenarios(&b, &[], 0);
        assert!(result.is_err());
    }

    #[test]
    fn invalid_probability_returns_error() {
        let b = beliefs(&[("A", 1.5)]);
        let result = sample_scenarios(&b, &[], 100);
        assert!(result.is_err());
    }

    #[test]
    fn certain_market_always_true() {
        let b = beliefs(&[("CERTAIN", 1.0)]);
        let batch = sample_scenarios(&b, &[], 200).unwrap();
        assert!(batch
            .scenarios
            .iter()
            .all(|s| *s.market_outcomes.get("CERTAIN").unwrap()));
    }

    #[test]
    fn impossible_market_always_false() {
        let b = beliefs(&[("IMPOSSIBLE", 0.0)]);
        let batch = sample_scenarios(&b, &[], 200).unwrap();
        assert!(batch
            .scenarios
            .iter()
            .all(|s| !s.market_outcomes.get("IMPOSSIBLE").unwrap()));
    }

    #[test]
    fn positive_dependency_raises_child_frequency() {
        // Parent always true (prob=1.0), child base=0.3, weight=+0.5
        // → effective child prob ≈ 0.8 → much higher than 0.3
        let b = beliefs(&[("PARENT", 1.0), ("CHILD", 0.3)]);
        let dep = DependencyEdge {
            parent: "PARENT".into(),
            child: "CHILD".into(),
            weight: 0.5,
        };
        let batch = sample_scenarios(&b, &[dep], 1_000).unwrap();
        let child_true = batch
            .scenarios
            .iter()
            .filter(|s| *s.market_outcomes.get("CHILD").unwrap())
            .count();
        let child_freq = child_true as f64 / 1_000.0;
        // Without dep ≈ 0.30; with dep ≈ 0.80.  Check clearly above baseline.
        assert!(
            child_freq > 0.60,
            "expected child freq > 0.60, got {child_freq:.3}"
        );
    }

    #[test]
    fn negative_dependency_lowers_child_frequency() {
        // Parent always true (prob=1.0), child base=0.8, weight=-0.5
        // → effective child prob ≈ 0.3 → much lower than 0.8
        let b = beliefs(&[("PARENT", 1.0), ("CHILD", 0.8)]);
        let dep = DependencyEdge {
            parent: "PARENT".into(),
            child: "CHILD".into(),
            weight: -0.5,
        };
        let batch = sample_scenarios(&b, &[dep], 1_000).unwrap();
        let child_true = batch
            .scenarios
            .iter()
            .filter(|s| *s.market_outcomes.get("CHILD").unwrap())
            .count();
        let child_freq = child_true as f64 / 1_000.0;
        assert!(
            child_freq < 0.55,
            "expected child freq < 0.55, got {child_freq:.3}"
        );
    }

    #[test]
    fn batch_timestamp_is_uniform() {
        // All scenarios in a batch must share the same timestamp (captured once).
        let b = beliefs(&[("A", 0.5)]);
        let batch = sample_scenarios(&b, &[], 50).unwrap();
        let ts = batch.scenarios[0].timestamp;
        assert!(
            batch.scenarios.iter().all(|s| s.timestamp == ts),
            "timestamps differ within batch"
        );
    }
}
