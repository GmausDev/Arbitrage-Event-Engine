// crates/scenario_engine/tests/scenario_tests.rs
//
// Integration tests for the Scenario Engine.
//
// Tests cover:
//  1. Scenario generation produces valid outcomes
//  2. Scenario expectations converge to probabilities (10k samples, <3% error)
//  3. Joint probability calculations are correct
//  4. Mispricing detection triggers signals
//  5. Large scenario batches remain stable
//  6. Dependency influence affects sampled outcomes
//  7. Engine handles repeated world updates without memory growth
//  8. Engine publishes ScenarioBatch and ScenarioExpectations on world update
//  9. Engine emits ScenarioSignal for clear mispricing

use std::collections::HashMap;
use std::time::Duration;

use common::{Event, EventBus, GraphUpdate, NodeProbUpdate, WorldProbabilityUpdate};
use scenario_engine::{
    ScenarioEngine, ScenarioEngineConfig,
    analysis::{compute_expectations, detect_mispricing, joint_prob_lookup},
    sampler::sample_scenarios,
    types::{DependencyEdge, ScenarioExpectation},
};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn beliefs(pairs: &[(&str, f64)]) -> HashMap<String, f64> {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

fn make_bus() -> EventBus {
    EventBus::new()
}

// ---------------------------------------------------------------------------
// Test 1: Scenario generation produces valid outcomes
// ---------------------------------------------------------------------------

#[test]
fn scenario_generation_produces_valid_outcomes() {
    let b = beliefs(&[("A", 0.3), ("B", 0.7), ("C", 0.5)]);
    let batch = sample_scenarios(&b, &[], 500).unwrap();

    assert_eq!(batch.sample_size, 500);
    assert_eq!(batch.scenarios.len(), 500);

    for scenario in &batch.scenarios {
        assert_eq!(scenario.market_outcomes.len(), 3, "missing market outcomes");
        assert!((scenario.probability_weight - 1.0 / 500.0).abs() < 1e-12);
    }

    // Weights sum to 1.0 (within float precision).
    let weight_sum: f64 = batch.scenarios.iter().map(|s| s.probability_weight).sum();
    assert!((weight_sum - 1.0).abs() < 1e-9);
}

// ---------------------------------------------------------------------------
// Test 2: Scenario expectations converge to probabilities
// ---------------------------------------------------------------------------

#[test]
fn expectations_converge_to_probabilities() {
    let b = beliefs(&[("X", 0.2), ("Y", 0.5), ("Z", 0.8)]);
    let batch = sample_scenarios(&b, &[], 10_000).unwrap();
    let exp = compute_expectations(&batch);

    for (market_id, &true_prob) in &b {
        let est = exp.expected_probabilities[market_id];
        assert!(
            (est - true_prob).abs() < 0.03,
            "market {market_id}: true={true_prob:.2}, estimated={est:.3} (diff > 3%)"
        );
    }
}

// ---------------------------------------------------------------------------
// Test 3: Joint probability calculations are correct
// ---------------------------------------------------------------------------

#[test]
fn joint_probabilities_are_correct_for_independent_markets() {
    // For independent markets: P(A∧B) ≈ P(A)·P(B)
    let b = beliefs(&[("A", 0.6), ("B", 0.4)]);
    let batch = sample_scenarios(&b, &[], 10_000).unwrap();
    let exp = compute_expectations(&batch);

    let pa = exp.expected_probabilities["A"];
    let pb = exp.expected_probabilities["B"];
    // Use joint_prob_lookup to test both orientations.
    let pab = joint_prob_lookup(&exp, "A", "B").expect("P(A∧B) missing");
    let pba = joint_prob_lookup(&exp, "B", "A").expect("P(B∧A) missing");

    // Symmetric.
    assert_eq!(pab, pba, "joint_prob_lookup must be symmetric");

    // Independent product rule (within 3%).
    let product = pa * pb;
    assert!(
        (pab - product).abs() < 0.03,
        "P(A∧B)={pab:.3} expected ≈ P(A)·P(B)={product:.3}"
    );

    // Sanity: pab ≤ min(pa, pb)
    assert!(pab <= pa + 1e-9);
    assert!(pab <= pb + 1e-9);
}

#[test]
fn joint_probabilities_respect_certain_market() {
    // If A is certain (prob=1), P(A∧B) = P(B).
    let b = beliefs(&[("A", 1.0), ("B", 0.5)]);
    let batch = sample_scenarios(&b, &[], 5_000).unwrap();
    let exp = compute_expectations(&batch);

    let pb = exp.expected_probabilities["B"];
    let pab = joint_prob_lookup(&exp, "A", "B").expect("P(A∧B) missing");

    assert!((pab - pb).abs() < 0.03, "P(A∧B) ≈ P(B) when P(A)=1");
}

#[test]
fn canonical_joint_pairs_stored_only_once() {
    // Canonical storage: only (A,B) where A < B; (B,A) not stored directly.
    let b = beliefs(&[("A", 0.5), ("B", 0.5)]);
    let batch = sample_scenarios(&b, &[], 500).unwrap();
    let exp = compute_expectations(&batch);

    // Exactly 1 canonical pair for 2 markets.
    assert_eq!(exp.joint_probabilities.len(), 1);
    // The stored key should be ("A","B") since "A" < "B".
    assert!(
        exp.joint_probabilities
            .contains_key(&("A".to_string(), "B".to_string())),
        "canonical pair (A,B) missing"
    );
    assert!(
        !exp.joint_probabilities
            .contains_key(&("B".to_string(), "A".to_string())),
        "symmetric duplicate (B,A) should not be stored"
    );
}

// ---------------------------------------------------------------------------
// Test 4: Mispricing detection triggers signals
// ---------------------------------------------------------------------------

#[test]
fn mispricing_detection_triggers_signal_above_threshold() {
    let mut expected_probs = HashMap::new();
    expected_probs.insert("MKT".to_string(), 0.80);

    let exp = ScenarioExpectation {
        expected_probabilities: expected_probs,
        joint_probabilities: HashMap::new(),
    };

    let mut market_prices = HashMap::new();
    market_prices.insert("MKT".to_string(), 0.55); // 25% gap

    let signals = detect_mispricing(&exp, &market_prices, &HashMap::new(), 0.10);

    assert_eq!(signals.len(), 1);
    let sig = &signals[0];
    assert_eq!(sig.market_id, "MKT");
    assert!((sig.expected_probability - 0.80).abs() < 1e-9);
    assert!((sig.market_probability - 0.55).abs() < 1e-9);
    assert!((sig.mispricing - 0.25).abs() < 1e-9);
}

#[test]
fn mispricing_below_threshold_produces_no_signal() {
    let mut expected_probs = HashMap::new();
    expected_probs.insert("MKT".to_string(), 0.52);

    let exp = ScenarioExpectation {
        expected_probabilities: expected_probs,
        joint_probabilities: HashMap::new(),
    };
    let mut market_prices = HashMap::new();
    market_prices.insert("MKT".to_string(), 0.50); // 2% gap, threshold 5%

    let signals = detect_mispricing(&exp, &market_prices, &HashMap::new(), 0.05);
    assert!(signals.is_empty());
}

// ---------------------------------------------------------------------------
// Test 5: Large scenario batches remain stable
// ---------------------------------------------------------------------------

#[test]
fn large_batch_is_stable() {
    let b = beliefs(&[
        ("A", 0.1), ("B", 0.5), ("C", 0.9),
        ("D", 0.3), ("E", 0.7),
    ]);
    let deps = vec![
        DependencyEdge { parent: "A".into(), child: "B".into(), weight: 0.3 },
        DependencyEdge { parent: "C".into(), child: "D".into(), weight: -0.2 },
    ];

    let batch = sample_scenarios(&b, &deps, 10_000).unwrap();
    assert_eq!(batch.scenarios.len(), 10_000);

    let exp = compute_expectations(&batch);
    for (_, &p) in &exp.expected_probabilities {
        assert!((0.0..=1.0).contains(&p), "expected prob {p} out of range");
    }
    for (_, &p) in &exp.joint_probabilities {
        assert!((0.0..=1.0).contains(&p), "joint prob {p} out of range");
    }
}

// ---------------------------------------------------------------------------
// Test 6: Dependency influence affects sampled outcomes
// ---------------------------------------------------------------------------

#[test]
fn positive_dependency_raises_child_joint_probability() {
    let b = beliefs(&[("PARENT", 0.9), ("CHILD", 0.2)]);
    let deps = vec![DependencyEdge {
        parent: "PARENT".into(),
        child: "CHILD".into(),
        weight: 0.5,
    }];

    let batch = sample_scenarios(&b, &deps, 5_000).unwrap();
    let exp = compute_expectations(&batch);

    let child_freq = exp.expected_probabilities["CHILD"];
    assert!(
        child_freq > 0.45,
        "dependency not lifting child: expected >0.45, got {child_freq:.3}"
    );
}

#[test]
fn negative_dependency_lowers_child_probability() {
    let b = beliefs(&[("PARENT", 0.9), ("CHILD", 0.8)]);
    let deps = vec![DependencyEdge {
        parent: "PARENT".into(),
        child: "CHILD".into(),
        weight: -0.5,
    }];

    let batch = sample_scenarios(&b, &deps, 5_000).unwrap();
    let exp = compute_expectations(&batch);

    let child_freq = exp.expected_probabilities["CHILD"];
    assert!(
        child_freq < 0.60,
        "negative dep not suppressing child: expected <0.60, got {child_freq:.3}"
    );
}

// ---------------------------------------------------------------------------
// Test 7: Engine handles repeated world updates without memory growth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn engine_no_memory_growth_on_repeated_updates() {
    let bus = make_bus();
    let config = ScenarioEngineConfig {
        sample_size: 100,
        ..Default::default()
    };
    let engine = ScenarioEngine::new(config, bus.clone()).unwrap();
    let state = engine.state();

    let cancel = CancellationToken::new();
    tokio::spawn(engine.run(cancel.child_token()));

    // Send 50 WorldProbabilityUpdates for the same market — world_probabilities
    // must stay at exactly 1 entry, not grow to 50.
    for i in 0..50u32 {
        let mut updated = HashMap::new();
        updated.insert("REPEATED".to_string(), 0.5 + i as f64 * 0.001);
        bus.publish(Event::WorldProbability(WorldProbabilityUpdate {
            source_market_id: "REPEATED".to_string(),
            updated_probabilities: updated,
            propagation_count: 0,
            timestamp: chrono::Utc::now(),
        }))
        .ok();

        // Also send GraphUpdates for the same (REPEATED → CHILD) edge —
        // dependencies must stay at exactly 1 entry, not grow to 50.
        bus.publish(Event::Graph(GraphUpdate {
            source_market_id: "REPEATED".to_string(),
            node_updates: vec![NodeProbUpdate {
                market_id: "CHILD".to_string(),
                old_implied_prob: 0.4,
                new_implied_prob: 0.5,
            }],
        }))
        .ok();
    }

    tokio::time::sleep(Duration::from_millis(300)).await;
    cancel.cancel();

    let snap = state.read().await;
    // REPEATED added via WorldProbabilityUpdate; CHILD is NOT added since
    // on_graph_update no longer writes to world_probabilities.
    assert_eq!(
        snap.world_probabilities.len(),
        1,
        "world_probabilities grew: {:?}",
        snap.world_probabilities.keys().collect::<Vec<_>>()
    );
    // The (REPEATED → CHILD) edge should be upserted, not duplicated.
    assert_eq!(
        snap.dependencies.len(),
        1,
        "dependencies grew to {}",
        snap.dependencies.len()
    );
}

// ---------------------------------------------------------------------------
// Test 8: Engine publishes ScenarioBatch and ScenarioExpectations on update
// ---------------------------------------------------------------------------

#[tokio::test]
async fn engine_publishes_batch_and_expectations_on_world_update() {
    let bus = make_bus();
    let mut verify_rx = bus.subscribe();

    let config = ScenarioEngineConfig {
        sample_size: 200,
        min_mispricing_threshold: 0.01,
        ..Default::default()
    };
    let engine = ScenarioEngine::new(config, bus.clone()).unwrap();

    let cancel = CancellationToken::new();
    tokio::spawn(engine.run(cancel.child_token()));

    let mut updated = HashMap::new();
    updated.insert("AAA".to_string(), 0.6);
    updated.insert("BBB".to_string(), 0.4);
    bus.publish(Event::WorldProbability(WorldProbabilityUpdate {
        source_market_id: "AAA".to_string(),
        updated_probabilities: updated,
        propagation_count: 1,
        timestamp: chrono::Utc::now(),
    }))
    .ok();

    let deadline = Duration::from_millis(1_000);
    let mut saw_batch = false;
    let mut saw_expectations = false;

    let _ = tokio::time::timeout(deadline, async {
        loop {
            match verify_rx.recv().await {
                Ok(ev) => match ev.as_ref() {
                    Event::ScenarioBatch(_) => saw_batch = true,
                    Event::ScenarioExpectations(_) => saw_expectations = true,
                    _ => {}
                },
                Err(_) => break,
            }
            if saw_batch && saw_expectations {
                break;
            }
        }
    })
    .await;

    cancel.cancel();

    assert!(saw_batch, "ScenarioBatch event not received");
    assert!(saw_expectations, "ScenarioExpectations event not received");
}

// ---------------------------------------------------------------------------
// Test 9: Engine emits ScenarioSignal for a clear mispricing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn engine_emits_scenario_signal_for_mispricing() {
    let bus = make_bus();
    let mut verify_rx = bus.subscribe();

    let config = ScenarioEngineConfig {
        sample_size: 2_000,
        min_mispricing_threshold: 0.10, // 10% threshold
        ..Default::default()
    };
    let engine = ScenarioEngine::new(config, bus.clone()).unwrap();
    let state = engine.state();

    let cancel = CancellationToken::new();
    tokio::spawn(engine.run(cancel.child_token()));

    // Pre-seed market price well below the world-model probability.
    {
        let mut s = state.write().await;
        s.market_prices.insert("SIG_MKT".to_string(), 0.30);
    }

    let mut updated = HashMap::new();
    updated.insert("SIG_MKT".to_string(), 0.70);
    bus.publish(Event::WorldProbability(WorldProbabilityUpdate {
        source_market_id: "SIG_MKT".to_string(),
        updated_probabilities: updated,
        propagation_count: 0,
        timestamp: chrono::Utc::now(),
    }))
    .ok();

    let deadline = Duration::from_millis(2_000);
    let mut found_signal = false;

    let _ = tokio::time::timeout(deadline, async {
        loop {
            match verify_rx.recv().await {
                Ok(ev) => {
                    if let Event::ScenarioSignal(sig) = ev.as_ref() {
                        if sig.market_id == "SIG_MKT" {
                            found_signal = true;
                            break;
                        }
                    }
                }
                Err(_) => break,
            }
        }
    })
    .await;

    cancel.cancel();
    assert!(found_signal, "ScenarioSignal for SIG_MKT not received");
}
