// crates/world_model/tests/world_model_tests.rs

use std::time::Duration;

use common::{Event, EventBus};
use tokio_util::sync::CancellationToken;
use world_model::{
    ConstraintType, LogicalConstraint, WorldDependency, WorldModelConfig, WorldModelEngine,
    WorldState,
};
use world_model::{
    constraints::check_constraints,
    inference::{log_odds_to_prob, prob_to_log_odds, propagate_update},
    types::{MarketVariable, WorldInferenceResult},
};

// ── Math helpers ─────────────────────────────────────────────────────────────

#[test]
fn prob_log_odds_roundtrip() {
    for &p in &[0.1, 0.25, 0.5, 0.75, 0.9] {
        let lo = prob_to_log_odds(p);
        let p2 = log_odds_to_prob(lo);
        assert!(
            (p - p2).abs() < 1e-10,
            "round-trip failed for p={p}: got {p2}"
        );
    }
}

#[test]
fn prob_to_log_odds_monotone() {
    let lo_low = prob_to_log_odds(0.3);
    let lo_high = prob_to_log_odds(0.7);
    assert!(lo_high > lo_low);
}

#[test]
fn log_odds_extreme_values_stay_in_unit_interval() {
    assert!(log_odds_to_prob(100.0) <= 1.0);
    assert!(log_odds_to_prob(-100.0) >= 0.0);
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_state_with_deps(markets: &[(&str, f64)], deps: Vec<WorldDependency>) -> WorldState {
    let variables = markets
        .iter()
        .map(|(id, prob)| (id.to_string(), MarketVariable::new(*id, *prob)))
        .collect();
    WorldState {
        variables,
        dependencies: deps,
        ..Default::default()
    }
}

fn make_state_with_probs(markets: &[(&str, f64)]) -> WorldState {
    make_state_with_deps(markets, vec![])
}

// ── Propagation — single hop ──────────────────────────────────────────────────

#[test]
fn single_hop_propagation() {
    // A → B with weight 0.8, confidence 1.0
    let mut state = make_state_with_deps(
        &[("A", 0.5), ("B", 0.5)],
        vec![WorldDependency {
            parent_market: "A".into(),
            child_market: "B".into(),
            weight: 0.8,
            lag_seconds: 0.0,
            confidence: 1.0,
        }],
    );

    let mut result = WorldInferenceResult::default();
    // Capture old_prob BEFORE writing new_prob — critical for correct delta.
    let old_a = state.variables["A"].current_probability; // 0.5
    state.variables.get_mut("A").unwrap().current_probability = 0.75;
    propagate_update(&mut state, "A", old_a, 0.75, &mut result, 4, 0.6, 1e-6).unwrap();

    // A must be at new prob.
    assert!((state.variables["A"].current_probability - 0.75).abs() < 1e-6);
    // B must have moved upward (positive weight, positive delta).
    assert!(state.variables["B"].current_probability > 0.5,
        "B={}, expected > 0.5", state.variables["B"].current_probability);
    // Result must contain both markets.
    assert!(result.updated_probabilities.contains_key("A"));
    assert!(result.updated_probabilities.contains_key("B"));
    // At least one propagation edge recorded.
    assert_eq!(result.influence_propagations.len(), 1);
}

#[test]
fn negative_weight_inverts_direction() {
    // A → B with negative weight: A up → B down.
    let mut state = make_state_with_deps(
        &[("A", 0.5), ("B", 0.5)],
        vec![WorldDependency {
            parent_market: "A".into(),
            child_market: "B".into(),
            weight: -0.8,
            lag_seconds: 0.0,
            confidence: 1.0,
        }],
    );

    let mut result = WorldInferenceResult::default();
    let old_a = state.variables["A"].current_probability;
    state.variables.get_mut("A").unwrap().current_probability = 0.75;
    propagate_update(&mut state, "A", old_a, 0.75, &mut result, 4, 0.6, 1e-6).unwrap();

    assert!(state.variables["B"].current_probability < 0.5,
        "B={}, expected < 0.5 with negative weight", state.variables["B"].current_probability);
}

// ── Propagation — multi-hop ───────────────────────────────────────────────────

#[test]
fn multi_hop_propagation() {
    // Chain: A → B → C
    let mut state = make_state_with_deps(
        &[("A", 0.5), ("B", 0.5), ("C", 0.5)],
        vec![
            WorldDependency {
                parent_market: "A".into(),
                child_market: "B".into(),
                weight: 1.0,
                lag_seconds: 0.0,
                confidence: 1.0,
            },
            WorldDependency {
                parent_market: "B".into(),
                child_market: "C".into(),
                weight: 1.0,
                lag_seconds: 0.0,
                confidence: 1.0,
            },
        ],
    );

    let mut result = WorldInferenceResult::default();
    let old_a = state.variables["A"].current_probability;
    state.variables.get_mut("A").unwrap().current_probability = 0.8;
    propagate_update(&mut state, "A", old_a, 0.8, &mut result, 4, 0.6, 1e-9).unwrap();

    // All three markets should be updated.
    assert!(result.updated_probabilities.contains_key("A"));
    assert!(result.updated_probabilities.contains_key("B"),
        "B should be in updated_probabilities after propagation from A");
    assert!(result.updated_probabilities.contains_key("C"),
        "C should be in updated_probabilities after 2-hop propagation from A");

    // Each hop attenuates, so A > B > C in terms of upward shift from 0.5.
    let pa = state.variables["A"].current_probability;
    let pb = state.variables["B"].current_probability;
    let pc = state.variables["C"].current_probability;
    assert!(pa > pb, "A ({pa}) should have moved more than B ({pb})");
    assert!(pb > pc, "B ({pb}) should have moved more than C ({pc})");
    assert!(pc > 0.5, "C should still be above 0.5");
}

#[test]
fn hop_limit_prevents_deep_propagation() {
    // Chain A→B→C→D with max_hops=1 — only A and B should be updated.
    let mut state = make_state_with_deps(
        &[("A", 0.5), ("B", 0.5), ("C", 0.5), ("D", 0.5)],
        vec![
            WorldDependency {
                parent_market: "A".into(),
                child_market: "B".into(),
                weight: 1.0,
                lag_seconds: 0.0,
                confidence: 1.0,
            },
            WorldDependency {
                parent_market: "B".into(),
                child_market: "C".into(),
                weight: 1.0,
                lag_seconds: 0.0,
                confidence: 1.0,
            },
            WorldDependency {
                parent_market: "C".into(),
                child_market: "D".into(),
                weight: 1.0,
                lag_seconds: 0.0,
                confidence: 1.0,
            },
        ],
    );

    let mut result = WorldInferenceResult::default();
    let old_a = state.variables["A"].current_probability;
    state.variables.get_mut("A").unwrap().current_probability = 0.8;
    propagate_update(&mut state, "A", old_a, 0.8, &mut result, 1, 1.0, 1e-9).unwrap();

    assert!(result.updated_probabilities.contains_key("A"));
    assert!(result.updated_probabilities.contains_key("B"));
    // C and D should NOT be updated because max_hops=1.
    assert!(
        !result.updated_probabilities.contains_key("C"),
        "C should not be reached with max_hops=1"
    );
    assert!(
        !result.updated_probabilities.contains_key("D"),
        "D should not be reached with max_hops=1"
    );
}

// ── No infinite propagation loops ─────────────────────────────────────────────

#[test]
fn cyclic_dependencies_do_not_loop_forever() {
    // Cycle: A → B → A  (visited set breaks the cycle)
    let mut state = make_state_with_deps(
        &[("A", 0.5), ("B", 0.5)],
        vec![
            WorldDependency {
                parent_market: "A".into(),
                child_market: "B".into(),
                weight: 0.5,
                lag_seconds: 0.0,
                confidence: 1.0,
            },
            WorldDependency {
                parent_market: "B".into(),
                child_market: "A".into(),
                weight: 0.5,
                lag_seconds: 0.0,
                confidence: 1.0,
            },
        ],
    );

    let mut result = WorldInferenceResult::default();
    let old_a = state.variables["A"].current_probability;
    state.variables.get_mut("A").unwrap().current_probability = 0.75;
    // This must terminate without hanging.
    propagate_update(&mut state, "A", old_a, 0.75, &mut result, 10, 0.8, 1e-9).unwrap();

    // Exactly one propagation edge (A→B); B→A is blocked by visited set.
    assert!(
        result.influence_propagations.len() <= 2,
        "cyclic graph should not loop: {} propagations recorded",
        result.influence_propagations.len()
    );
}

// ── Stability under repeated updates ─────────────────────────────────────────

#[test]
fn stability_under_repeated_updates() {
    let mut state = make_state_with_deps(
        &[("A", 0.5), ("B", 0.5)],
        vec![WorldDependency {
            parent_market: "A".into(),
            child_market: "B".into(),
            weight: 0.3,
            lag_seconds: 0.0,
            confidence: 0.8,
        }],
    );

    // Simulate a sequence of updates: A moves to 0.6 on the first call, then
    // stays at 0.6.  Subsequent calls should produce zero delta and not change B.
    let target = 0.6_f64;
    let mut prev_b = state.variables["B"].current_probability;

    for i in 0..10 {
        let old_a = state.variables["A"].current_probability;
        state.variables.get_mut("A").unwrap().current_probability = target;
        let mut result = WorldInferenceResult::default();
        propagate_update(&mut state, "A", old_a, target, &mut result, 4, 0.6, 1e-9).unwrap();

        let new_b = state.variables["B"].current_probability;
        let change = (new_b - prev_b).abs();
        if i > 0 {
            // After the first iteration A is already at target, so delta = 0
            // and B should not change further.
            assert!(
                change < 1e-9,
                "iteration {i}: B changed by {change}, expected convergence after first update"
            );
        }
        prev_b = new_b;
    }

    let pa = state.variables["A"].current_probability;
    let pb = state.variables["B"].current_probability;
    assert!(pa >= 0.0 && pa <= 1.0, "A probability out of range: {pa}");
    assert!(pb >= 0.0 && pb <= 1.0, "B probability out of range: {pb}");
    // B should have moved from 0.5 towards 0.6 (positive weight).
    assert!(pb > 0.5, "B should be above 0.5 after propagation");
}

// ── Constraints — mutual exclusion ───────────────────────────────────────────

#[test]
fn mutual_exclusion_no_violation() {
    let mut state = make_state_with_probs(&[("A", 0.3), ("B", 0.3), ("C", 0.4)]);
    state.constraints.push(LogicalConstraint {
        constraint_id: "me1".into(),
        constraint_type: ConstraintType::MutualExclusion,
        markets_involved: vec!["A".into(), "B".into(), "C".into()],
        tolerance: 0.05,
    });
    let violations = check_constraints(&state);
    assert!(violations.is_empty(), "sum=1.0 should not violate");
}

#[test]
fn mutual_exclusion_violation_detected() {
    // A + B + C = 1.5 — clear violation.
    let mut state = make_state_with_probs(&[("A", 0.5), ("B", 0.5), ("C", 0.5)]);
    state.constraints.push(LogicalConstraint {
        constraint_id: "me2".into(),
        constraint_type: ConstraintType::MutualExclusion,
        markets_involved: vec!["A".into(), "B".into(), "C".into()],
        tolerance: 0.05,
    });
    let violations = check_constraints(&state);
    assert_eq!(violations.len(), 1);
    assert!((violations[0].violation_magnitude - 0.5).abs() < 1e-6);
}

// ── Constraints — AND ─────────────────────────────────────────────────────────

#[test]
fn and_constraint_no_violation() {
    // P(A AND B) = 0.2 ≤ min(P(A)=0.4, P(B)=0.5) = 0.4 — OK.
    let mut state = make_state_with_probs(&[("A", 0.4), ("B", 0.5), ("A_and_B", 0.2)]);
    state.constraints.push(LogicalConstraint {
        constraint_id: "and1".into(),
        constraint_type: ConstraintType::And,
        markets_involved: vec!["A".into(), "B".into(), "A_and_B".into()],
        tolerance: 0.01,
    });
    let violations = check_constraints(&state);
    assert!(violations.is_empty());
}

#[test]
fn and_constraint_violation_detected() {
    // P(A AND B) = 0.6 > min(0.4, 0.5) = 0.4 — violation of 0.2.
    let mut state = make_state_with_probs(&[("A", 0.4), ("B", 0.5), ("A_and_B", 0.6)]);
    state.constraints.push(LogicalConstraint {
        constraint_id: "and2".into(),
        constraint_type: ConstraintType::And,
        markets_involved: vec!["A".into(), "B".into(), "A_and_B".into()],
        tolerance: 0.01,
    });
    let violations = check_constraints(&state);
    assert_eq!(violations.len(), 1);
    assert!((violations[0].violation_magnitude - 0.2).abs() < 1e-6);
}

// ── Constraints — OR ──────────────────────────────────────────────────────────

#[test]
fn or_constraint_no_violation() {
    // P(A OR B) = 0.7 ≥ max(P(A)=0.4, P(B)=0.5) = 0.5 — OK.
    let mut state = make_state_with_probs(&[("A", 0.4), ("B", 0.5), ("A_or_B", 0.7)]);
    state.constraints.push(LogicalConstraint {
        constraint_id: "or1".into(),
        constraint_type: ConstraintType::Or,
        markets_involved: vec!["A".into(), "B".into(), "A_or_B".into()],
        tolerance: 0.01,
    });
    let violations = check_constraints(&state);
    assert!(violations.is_empty());
}

#[test]
fn or_constraint_violation_detected() {
    // P(A OR B) = 0.3 < max(0.4, 0.5) = 0.5 — violation of 0.2.
    let mut state = make_state_with_probs(&[("A", 0.4), ("B", 0.5), ("A_or_B", 0.3)]);
    state.constraints.push(LogicalConstraint {
        constraint_id: "or2".into(),
        constraint_type: ConstraintType::Or,
        markets_involved: vec!["A".into(), "B".into(), "A_or_B".into()],
        tolerance: 0.01,
    });
    let violations = check_constraints(&state);
    assert_eq!(violations.len(), 1);
    assert!((violations[0].violation_magnitude - 0.2).abs() < 1e-6);
}

// ── Constraint — missing markets skipped ─────────────────────────────────────

#[test]
fn constraint_skipped_when_market_missing() {
    let mut state = make_state_with_probs(&[("A", 0.5)]); // "B" not in state
    state.constraints.push(LogicalConstraint {
        constraint_id: "skip1".into(),
        constraint_type: ConstraintType::MutualExclusion,
        markets_involved: vec!["A".into(), "B".into()],
        tolerance: 0.05,
    });
    let violations = check_constraints(&state);
    assert!(violations.is_empty(), "missing market should skip the constraint");
}

// ── add_dependency deduplication ─────────────────────────────────────────────

#[test]
fn add_dependency_replaces_duplicate_edge() {
    let mut state = WorldState::default();
    let dep1 = WorldDependency {
        parent_market: "A".into(),
        child_market: "B".into(),
        weight: 0.5,
        lag_seconds: 0.0,
        confidence: 1.0,
    };
    let dep2 = WorldDependency {
        parent_market: "A".into(),
        child_market: "B".into(),
        weight: 0.9,
        lag_seconds: 0.0,
        confidence: 0.8,
    };
    assert!(state.add_dependency(dep1).unwrap().is_none());
    let replaced = state.add_dependency(dep2).unwrap();
    assert!(replaced.is_some(), "should return the old dep");
    assert!((replaced.unwrap().weight - 0.5).abs() < 1e-9);
    assert_eq!(state.dependencies.len(), 1, "should not have duplicate edges");
    assert!((state.dependencies[0].weight - 0.9).abs() < 1e-9);
}

// ── Integration — engine emits WorldProbabilityUpdate via bus ─────────────────

#[tokio::test]
async fn engine_emits_world_probability_on_posterior() {
    let bus = EventBus::new();

    // Subscribe BEFORE spawning the engine to avoid the subscribe-after-spawn race.
    let mut verify_rx = bus.subscribe();

    let config = WorldModelConfig {
        min_signal_edge: 0.0, // emit signals regardless of edge size
        ..WorldModelConfig::default()
    };
    let engine = WorldModelEngine::new(config, bus.clone()).unwrap();

    let cancel = CancellationToken::new();
    tokio::spawn(engine.run(cancel.child_token()));

    // Publish a PosteriorUpdate.
    bus.publish(Event::Posterior(common::PosteriorUpdate {
        market_id: "TEST_MARKET".into(),
        prior_prob: 0.5,
        posterior_prob: 0.72,
        market_prob: Some(0.55),
        deviation: 0.17,
        confidence: 0.8,
        timestamp: chrono::Utc::now(),
    }))
    .unwrap();

    // Wait for the engine to emit at least one WorldProbability event.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let mut got_world_prob = false;
    let mut got_world_signal = false;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, verify_rx.recv()).await {
            Ok(Ok(ev)) => match ev.as_ref() {
                Event::WorldProbability(wu) => {
                    assert_eq!(wu.source_market_id, "TEST_MARKET");
                    assert!(wu.updated_probabilities.contains_key("TEST_MARKET"));
                    got_world_prob = true;
                }
                Event::WorldSignal(ws) => {
                    assert_eq!(ws.market_id, "TEST_MARKET");
                    // Edge should be world_prob − market_prob = 0.72 − 0.55 ≈ 0.17.
                    assert!(ws.world_edge > 0.0, "expected positive edge");
                    got_world_signal = true;
                }
                _ => {}
            },
            _ => break,
        }
        if got_world_prob && got_world_signal {
            break;
        }
    }

    cancel.cancel();
    assert!(got_world_prob, "expected Event::WorldProbability to be emitted");
    assert!(got_world_signal, "expected Event::WorldSignal to be emitted");
}

// ── Integration — engine propagates through dependencies ─────────────────────

#[tokio::test]
async fn engine_propagates_through_dependencies() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();

    let config = WorldModelConfig {
        min_signal_edge: 0.0,
        propagation_threshold: 1e-9,
        ..WorldModelConfig::default()
    };
    let engine = WorldModelEngine::new(config, bus.clone()).unwrap();

    // Pre-seed: A → B dependency, both starting at 0.5.
    {
        let state = engine.state();
        let mut st = state.write().await;
        st.variables.insert("A".into(), MarketVariable::new("A", 0.5));
        st.variables.insert("B".into(), {
            let mut v = MarketVariable::new("B", 0.5);
            v.market_probability = Some(0.5); // give B a market price for WorldSignal
            v
        });
        st.add_dependency(WorldDependency {
            parent_market: "A".into(),
            child_market: "B".into(),
            weight: 0.8,
            lag_seconds: 0.0,
            confidence: 1.0,
        }).unwrap();
    }

    let cancel = CancellationToken::new();
    tokio::spawn(engine.run(cancel.child_token()));

    // Posterior moves A from 0.5 to 0.8 — should propagate to B.
    bus.publish(Event::Posterior(common::PosteriorUpdate {
        market_id: "A".into(),
        prior_prob: 0.5,
        posterior_prob: 0.8,
        market_prob: Some(0.5),
        deviation: 0.3,
        confidence: 0.9,
        timestamp: chrono::Utc::now(),
    }))
    .unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let mut b_was_updated = false;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, verify_rx.recv()).await {
            Ok(Ok(ev)) => {
                if let Event::WorldProbability(wu) = ev.as_ref() {
                    if wu.updated_probabilities.contains_key("B") {
                        let b_prob = wu.updated_probabilities["B"];
                        assert!(
                            b_prob > 0.5,
                            "B should have moved above 0.5 via propagation, got {b_prob}"
                        );
                        b_was_updated = true;
                        break;
                    }
                }
            }
            _ => break,
        }
    }

    cancel.cancel();
    assert!(
        b_was_updated,
        "expected B to appear in WorldProbabilityUpdate after A's posterior propagated through dependency"
    );
}

// ── Integration — constraint violation emitted on bus ─────────────────────────

#[tokio::test]
async fn engine_emits_inconsistency_on_constraint_violation() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();

    let config = WorldModelConfig::default();
    let engine = WorldModelEngine::new(config, bus.clone()).unwrap();

    // Pre-seed the world state: add two markets and a mutual exclusion constraint
    // so that when both hit high probabilities we get a violation.
    {
        let state = engine.state();
        let mut st = state.write().await;
        st.variables.insert(
            "CAND_A".into(),
            MarketVariable::new("CAND_A", 0.6),
        );
        st.variables.insert(
            "CAND_B".into(),
            MarketVariable::new("CAND_B", 0.6),
        );
        st.constraints.push(LogicalConstraint {
            constraint_id: "election".into(),
            constraint_type: ConstraintType::MutualExclusion,
            markets_involved: vec!["CAND_A".into(), "CAND_B".into()],
            tolerance: 0.05,
        });
    }

    let cancel = CancellationToken::new();
    tokio::spawn(engine.run(cancel.child_token()));

    // Send a posterior for CAND_A to trigger inference + constraint check.
    // Both CAND_A(0.6) + CAND_B(0.6) = 1.2 → violation of 0.2.
    bus.publish(Event::Posterior(common::PosteriorUpdate {
        market_id: "CAND_A".into(),
        prior_prob: 0.5,
        posterior_prob: 0.6,
        market_prob: Some(0.55),
        deviation: 0.05,
        confidence: 0.7,
        timestamp: chrono::Utc::now(),
    }))
    .unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let mut got_inconsistency = false;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, verify_rx.recv()).await {
            Ok(Ok(ev)) => {
                if let Event::Inconsistency(inc) = ev.as_ref() {
                    assert_eq!(inc.constraint_id, "election");
                    assert!(inc.violation_magnitude > 0.05);
                    got_inconsistency = true;
                    break;
                }
            }
            _ => break,
        }
    }

    cancel.cancel();
    assert!(got_inconsistency, "expected Event::Inconsistency to be emitted");
}
