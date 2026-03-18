// crates/market_graph/tests/propagation.rs
//
// Integration tests for the BFS depth-limited propagation algorithm.
//
// All tests use the public API of `market_graph` to ensure the published
// interface behaves correctly end-to-end.
//
// Graph topology used throughout (unless noted otherwise):
//
//   A ──(w=0.8)──► B ──(w=0.8)──► C ──(w=0.8)──► D
//
// This gives a 4-node linear chain: A is always the updated root.

use std::sync::Arc;
use std::time::Duration;

use common::{Event, EventBus};
use market_graph::{
    EdgeType, GraphConfig, MarketGraph, MarketGraphEngine, MarketNode, PropagationConfig,
    UpdateSource,
};
use tokio_util::sync::CancellationToken;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn node(id: &str, prob: f64) -> MarketNode {
    MarketNode::new(id, id, prob, None, None)
}

/// Build a linear chain A → B → C → D with weight `w` on every edge.
fn chain_engine(w: f64) -> MarketGraphEngine {
    let mut e = MarketGraphEngine::new();
    for id in ["a", "b", "c", "d"] {
        e.add_market(node(id, 0.50));
    }
    for pair in [("a", "b"), ("b", "c"), ("c", "d")] {
        e.add_dependency(pair.0, pair.1, w, 1.0, EdgeType::Causal).unwrap();
    }
    e
}

// ── 1. No propagation when delta < threshold ──────────────────────────────────

/// When the probability change is below the configured `change_threshold`,
/// the engine must not update any downstream node.
#[test]
fn no_propagation_below_threshold() {
    let mut e = MarketGraphEngine::with_config(PropagationConfig {
        change_threshold: 0.05,
        ..PropagationConfig::default()
    });
    e.add_market(node("a", 0.50));
    e.add_market(node("b", 0.50));
    e.add_dependency("a", "b", 0.8, 1.0, EdgeType::Causal).unwrap();

    // Move A by only 0.02 — below the 0.05 threshold.
    let result = e.update_and_propagate("a", 0.52, UpdateSource::Api).unwrap();

    assert_eq!(result.nodes_updated, 0, "no nodes should be updated");
    assert_eq!(result.iterations, 0, "no iterations should run");
    assert!(result.node_changes.is_empty());

    // B's implied_prob must be unchanged.
    assert!(
        (e.get_implied_prob("b").unwrap() - 0.50).abs() < 1e-9,
        "b implied_prob must not change below threshold"
    );
}

// ── 2. Correct propagation when delta ≥ threshold ────────────────────────────

/// When the change exceeds `change_threshold`, downstream nodes are updated
/// with the correct damped implied probabilities.
#[test]
fn propagation_above_threshold() {
    let mut e = chain_engine(0.8); // A→B→C→D, all weights 0.8

    // Move A by 0.20 — well above the default threshold of 0.005.
    let result = e.update_and_propagate("a", 0.70, UpdateSource::Api).unwrap();

    // B, C should be updated (D is at depth 3 = max_depth, so skipped by default).
    assert!(result.nodes_updated >= 2, "at least B and C must be updated");

    let b = e.get_implied_prob("b").unwrap();
    let c = e.get_implied_prob("c").unwrap();

    // B: 0.50 + 0.8 × 0.20 × 0.85 (damping) ≈ 0.636
    let expected_b = 0.50 + 0.8 * 0.20 * 0.85;
    assert!(
        (b - expected_b).abs() < 1e-4,
        "b implied_prob expected ≈{expected_b:.4}, got {b:.4}"
    );

    // C: receives B's actual_change (≈ 0.136) attenuated by one more hop.
    assert!(c > 0.50, "c must be updated above baseline");
    assert!(b > c, "signal must attenuate A→B→C");

    // node_changes must contain B and C market IDs.
    let changed_ids: Vec<&str> =
        result.node_changes.iter().map(|c| c.market_id.as_str()).collect();
    assert!(changed_ids.contains(&"b"), "b missing from node_changes");
    assert!(changed_ids.contains(&"c"), "c missing from node_changes");

    // Each NodeChange must have a valid old→new direction.
    for nc in &result.node_changes {
        assert!(
            nc.new_implied_prob > nc.old_implied_prob,
            "{} should have risen; old={:.4} new={:.4}",
            nc.market_id,
            nc.old_implied_prob,
            nc.new_implied_prob
        );
    }
}

// ── 3. Propagation stops at max_depth ────────────────────────────────────────

/// With `max_depth = 1`, only direct neighbours of the updated node (B) should
/// be updated.  C and D must remain at their initial implied probability.
#[test]
fn propagation_stops_at_max_depth() {
    let mut e = MarketGraphEngine::with_config(PropagationConfig {
        max_depth: 1,
        ..PropagationConfig::default()
    });
    for id in ["a", "b", "c", "d"] {
        e.add_market(node(id, 0.50));
    }
    for (from, to) in [("a", "b"), ("b", "c"), ("c", "d")] {
        e.add_dependency(from, to, 0.8, 1.0, EdgeType::Causal).unwrap();
    }

    let result = e.update_and_propagate("a", 0.70, UpdateSource::Api).unwrap();

    // Only B should be updated at depth 1.
    let changed_ids: Vec<&str> =
        result.node_changes.iter().map(|c| c.market_id.as_str()).collect();
    assert!(changed_ids.contains(&"b"), "b must be updated at depth 1");
    assert!(!changed_ids.contains(&"c"), "c must NOT be updated (depth 2 > max_depth=1)");
    assert!(!changed_ids.contains(&"d"), "d must NOT be updated (depth 3 > max_depth=1)");

    // converged should be false because tasks were depth-limited.
    assert!(!result.converged, "converged must be false when depth limit was hit");

    // C and D's implied_probs must be unchanged.
    assert!(
        (e.get_implied_prob("c").unwrap() - 0.50).abs() < 1e-9,
        "c implied_prob must not change"
    );
    assert!(
        (e.get_implied_prob("d").unwrap() - 0.50).abs() < 1e-9,
        "d implied_prob must not change"
    );
}

/// With `max_depth = 2`, B and C are updated, D is not.
#[test]
fn propagation_stops_at_max_depth_2() {
    let mut e = MarketGraphEngine::with_config(PropagationConfig {
        max_depth: 2,
        ..PropagationConfig::default()
    });
    for id in ["a", "b", "c", "d"] {
        e.add_market(node(id, 0.50));
    }
    for (from, to) in [("a", "b"), ("b", "c"), ("c", "d")] {
        e.add_dependency(from, to, 0.8, 1.0, EdgeType::Causal).unwrap();
    }

    let result = e.update_and_propagate("a", 0.70, UpdateSource::Api).unwrap();

    let changed_ids: Vec<&str> =
        result.node_changes.iter().map(|c| c.market_id.as_str()).collect();
    assert!(changed_ids.contains(&"b"), "b must be updated at depth 1");
    assert!(changed_ids.contains(&"c"), "c must be updated at depth 2");
    assert!(!changed_ids.contains(&"d"), "d must NOT be updated (depth 3 > max_depth=2)");
    assert!(!result.converged, "converged must be false when depth limit was hit");
}

// ── 4. Frontier queue visits expected nodes only ─────────────────────────────

/// In a diamond graph (A→B, A→C, B→D, C→D, D→E), with `max_depth = 2`:
///
/// - Depth 0: A (source, not in downstream results)
/// - Depth 1: B, C  ← updated when A is processed
/// - Depth 2: D     ← updated when B and C are processed; its task is queued
///                     at depth 2 but is dropped before it can propagate to E
/// - Depth 3: E     ← D's task is dropped (depth 2 >= max_depth=2), so E is NEVER updated
/// - z (no path): never touched
#[test]
fn frontier_queue_diamond_visits_correct_nodes() {
    //   A
    //  / \
    // B   C
    //  \ /
    //   D
    //   |
    //   E  ← must NOT be updated (D's task is depth-limited)
    //
    //   z  ← no path from A, must not be touched
    let mut e = MarketGraphEngine::with_config(PropagationConfig {
        max_depth: 2,   // A(0)→B(1)→D(2); D's task is dropped so E is safe
        ..PropagationConfig::default()
    });
    for id in ["a", "b", "c", "d", "e", "z"] {
        e.add_market(node(id, 0.50));
    }
    e.add_dependency("a", "b", 0.7, 1.0, EdgeType::Causal).unwrap();
    e.add_dependency("a", "c", 0.7, 1.0, EdgeType::Causal).unwrap();
    e.add_dependency("b", "d", 0.7, 1.0, EdgeType::Causal).unwrap();
    e.add_dependency("c", "d", 0.7, 1.0, EdgeType::Causal).unwrap();
    e.add_dependency("d", "e", 0.7, 1.0, EdgeType::Causal).unwrap();

    let result = e.update_and_propagate("a", 0.70, UpdateSource::Api).unwrap();

    let changed_ids: Vec<&str> =
        result.node_changes.iter().map(|c| c.market_id.as_str()).collect();

    // B and C are depth-1 (direct neighbours of A) — must be updated.
    assert!(changed_ids.contains(&"b"), "b must be updated (depth 1)");
    assert!(changed_ids.contains(&"c"), "c must be updated (depth 1)");

    // D is depth-2; B and C process their outgoing edges (including D) before
    // being depth-limited.  D's task is then queued at depth 2 and immediately
    // dropped (2 >= max_depth=2), so D is updated but doesn't propagate to E.
    assert!(changed_ids.contains(&"d"), "d must be updated (written by B and C at depth 1)");

    // E must NOT be updated — D's task was dropped so the D→E edge was never walked.
    assert!(
        !changed_ids.contains(&"e"),
        "e must NOT be updated (D's task is depth-limited)"
    );
    // z has no path from A and must never appear.
    assert!(
        !changed_ids.contains(&"z"),
        "z has no path from a and must not be updated"
    );

    // depth_limited = true because D's task was dropped.
    assert!(!result.converged, "converged must be false when tasks are depth-limited");
}

/// Isolated nodes (no path from the updated root) are never touched,
/// regardless of graph size.
#[test]
fn frontier_queue_does_not_visit_unreachable_nodes() {
    let mut e = chain_engine(0.8); // A→B→C→D

    // Add a completely disconnected market.
    e.add_market(node("isolated", 0.42));

    let result = e.update_and_propagate("a", 0.70, UpdateSource::Api).unwrap();

    let changed_ids: Vec<&str> =
        result.node_changes.iter().map(|c| c.market_id.as_str()).collect();
    assert!(
        !changed_ids.contains(&"isolated"),
        "isolated node must not appear in node_changes"
    );
    // Sanity: isolated node's implied_prob must be unchanged.
    assert!(
        (e.get_implied_prob("isolated").unwrap() - 0.42).abs() < 1e-9
    );
}

// ── 5. GraphUpdate events are emitted correctly ───────────────────────────────

/// The async `MarketGraph` wrapper must publish a `GraphUpdate` event
/// containing correct `source_market_id` and `node_updates` after each
/// significant `MarketUpdate`.
#[tokio::test]
async fn graph_update_event_emitted_correctly() {
    let bus = EventBus::new();
    let cancel = CancellationToken::new();

    // Pre-seed the engine with a 3-node chain A→B→C.
    let mut raw_engine = MarketGraphEngine::new();
    raw_engine.add_market(node("a", 0.50));
    raw_engine.add_market(node("b", 0.50));
    raw_engine.add_market(node("c", 0.50));
    raw_engine.add_dependency("a", "b", 0.8, 1.0, EdgeType::Causal).unwrap();
    raw_engine.add_dependency("b", "c", 0.8, 1.0, EdgeType::Causal).unwrap();

    let shared_engine = Arc::new(tokio::sync::RwLock::new(raw_engine));
    let graph = MarketGraph::with_engine(Arc::clone(&shared_engine), bus.clone());

    // Subscribe *before* spawning the graph runner so we don't miss the
    // GraphUpdate that will be published after the runner processes our event.
    let mut rx = bus.subscribe();
    tokio::spawn(graph.run(cancel.child_token()));

    // Give the graph runner a moment to enter its recv loop and subscribe to
    // the bus before we publish the triggering MarketUpdate.
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Publish a MarketUpdate for A with a meaningful probability change.
    let wire_a = common::MarketNode {
        id:          "a".into(),
        probability: 0.70,          // Δ = 0.20 — well above threshold
        liquidity:   10_000.0,
        last_update: chrono::Utc::now(),
    };
    bus.publish(Event::Market(common::MarketUpdate { market: wire_a })).unwrap();

    // Wait for the GraphUpdate event (or timeout).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let mut graph_update_received = false;

    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Ok(ev)) => {
                if let Event::Graph(gu) = ev.as_ref() {
                    // Must originate from A.
                    assert_eq!(gu.source_market_id, "a");
                    // Must contain at least B (direct neighbour of A).
                    let updated: Vec<&str> =
                        gu.node_updates.iter().map(|u| u.market_id.as_str()).collect();
                    assert!(
                        updated.contains(&"b"),
                        "GraphUpdate must include b; got {updated:?}"
                    );
                    // Every NodeProbUpdate must have new > old (positive delta).
                    for nu in &gu.node_updates {
                        assert!(
                            nu.new_implied_prob > nu.old_implied_prob,
                            "{} should have risen: old={:.4} new={:.4}",
                            nu.market_id,
                            nu.old_implied_prob,
                            nu.new_implied_prob
                        );
                    }
                    graph_update_received = true;
                    break;
                }
            }
            _ => continue,
        }
    }

    cancel.cancel();
    assert!(graph_update_received, "timed out waiting for GraphUpdate event");
}

/// When the probability change is below the threshold, NO GraphUpdate event
/// should be published (only a MarketUpdate is on the bus).
#[tokio::test]
async fn no_graph_update_below_threshold() {
    let bus = EventBus::new();
    let cancel = CancellationToken::new();

    // High threshold so tiny moves don't propagate.
    let cfg = GraphConfig { change_threshold: 0.10, max_depth: 3 };
    let graph = MarketGraph::from_config(cfg, bus.clone());

    // Subscribe before spawning so we don't miss events.
    let mut rx = bus.subscribe();
    tokio::spawn(graph.run(cancel.child_token()));

    // Publish a market with a tiny move (0.02 < 0.10 threshold).
    let wire = common::MarketNode {
        id:          "x".into(),
        probability: 0.52,   // first sighting — treated as new node, no GraphUpdate
        liquidity:   5_000.0,
        last_update: chrono::Utc::now(),
    };
    bus.publish(Event::Market(common::MarketUpdate { market: wire.clone() })).unwrap();

    // Second publish: tiny Δ = 0.52 → 0.53 (0.01 < 0.10 threshold)
    let wire2 = common::MarketNode { probability: 0.53, ..wire };
    bus.publish(Event::Market(common::MarketUpdate { market: wire2 })).unwrap();

    // Drain the bus for a short window; must NOT see any GraphUpdate.
    let mut saw_graph_update = false;
    let window = tokio::time::Instant::now() + Duration::from_millis(300);
    while tokio::time::Instant::now() < window {
        match tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
            Ok(Ok(ev)) if matches!(ev.as_ref(), Event::Graph(_)) => {
                saw_graph_update = true;
                break;
            }
            _ => {}
        }
    }

    cancel.cancel();
    assert!(!saw_graph_update, "GraphUpdate must not be emitted for sub-threshold change");
}
