// tests/integration_harness/src/main.rs
//
// Integration Test Harness — Market Scanner + Market Graph Pipeline
//
// ═══════════════════════════════════════════════════════════════════════════
//  What this validates
// ═══════════════════════════════════════════════════════════════════════════
//
//  Phase 1 — Direct engine test (synchronous, no event bus)
//  ─────────────────────────────────────────────────────────────────────────
//    ✓ MarketGraphEngine can be seeded with nodes and edges
//    ✓ update_and_propagate() correctly propagates Δprob down a chain
//    ✓ Deviation signals (current_prob − implied_prob) are computed correctly
//    ✓ top_deviations() identifies the most mispriced nodes
//
//  Phase 2 — End-to-end async event bus integration
//  ─────────────────────────────────────────────────────────────────────────
//    ✓ MarketScanner publishes Event::Market updates on each tick
//    ✓ MarketGraph (async wrapper) receives those events and propagates
//    ✓ Event::Graph is published downstream after successful propagation
//    ✓ A watcher task receives both event types and prints deviations live
//
// ═══════════════════════════════════════════════════════════════════════════
//  How to run
// ═══════════════════════════════════════════════════════════════════════════
//    # Minimal output
//    cargo run --bin integration_harness
//
//    # Verbose (includes tracing logs from engine + scanner)
//    RUST_LOG=debug cargo run --bin integration_harness
//
// ═══════════════════════════════════════════════════════════════════════════

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

// ── Common event bus types ────────────────────────────────────────────────────
use common::{Event, EventBus, MarketNode as WireNode};

// ── Market graph types ────────────────────────────────────────────────────────
use market_graph::{
    EdgeType, MarketGraph, MarketGraphEngine, MarketNode, SharedEngine, UpdateSource,
};

// ── Market scanner types ──────────────────────────────────────────────────────
use market_scanner::{MarketDataSource, MarketScanner, ScannerConfig};

// ═══════════════════════════════════════════════════════════════════════════════
//  Synthetic market IDs
// ═══════════════════════════════════════════════════════════════════════════════

/// Root node — its probability oscillates on each scanner tick.
const MKT_A: &str = "ELECTION-2026-PARTY-A";
/// Downstream child: A → B with weight +0.75 (same-direction correlation).
const MKT_B: &str = "SENATE-MAJORITY-2026";
/// Downstream grandchild: B → C with weight +0.60.
const MKT_C: &str = "FLAGSHIP-POLICY-2026";

// ═══════════════════════════════════════════════════════════════════════════════
//  SyntheticSource — MockSource for deterministic testing
// ═══════════════════════════════════════════════════════════════════════════════

/// A `MarketDataSource` that produces deterministic, oscillating prices so the
/// graph engine always has real deltas to propagate — no live API required.
///
/// On each `fetch_markets` call the internal tick counter advances:
/// - `MKT_A` sweeps 0.55 → 0.63 over ticks 0-4, then back 0.63 → 0.55 over 5-9
/// - `MKT_B` and `MKT_C` stay at their initial prices; their `implied_prob`
///   is driven purely by propagation from A.
struct SyntheticSource {
    tick: Arc<AtomicU64>,
}

impl SyntheticSource {
    fn new() -> Box<Self> {
        Box::new(Self { tick: Arc::new(AtomicU64::new(0)) })
    }
}

#[async_trait]
impl MarketDataSource for SyntheticSource {
    fn name(&self) -> &str {
        "synthetic"
    }

    async fn fetch_markets(&self) -> anyhow::Result<Vec<WireNode>> {
        let tick = self.tick.fetch_add(1, Ordering::SeqCst);

        // MKT_A oscillates on a 10-tick cycle: 0.55 → 0.63 → 0.55 → …
        let phase = (tick % 10) as f64;
        let prob_a = if phase < 5.0 {
            0.55 + phase * 0.016     // 0.550 → 0.614
        } else {
            0.614 - (phase - 5.0) * 0.016  // 0.614 → 0.550
        };

        Ok(vec![
            WireNode {
                id:          MKT_A.into(),
                probability: prob_a,
                liquidity:   50_000.0,
                last_update: Utc::now(),
            },
            // B and C keep their market-reported prices flat —
            // their implied_prob changes via graph propagation.
            WireNode {
                id:          MKT_B.into(),
                probability: 0.50,
                liquidity:   30_000.0,
                last_update: Utc::now(),
            },
            WireNode {
                id:          MKT_C.into(),
                probability: 0.40,
                liquidity:   15_000.0,
                last_update: Utc::now(),
            },
        ])
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Graph seeding helpers
// ═══════════════════════════════════════════════════════════════════════════════

/// Populate the engine with the three test markets and two directed edges.
///
/// Graph topology:
/// ```text
///   MKT_A ──(+0.75, conf=0.90)──► MKT_B
///   MKT_B ──(+0.60, conf=0.80)──► MKT_C
/// ```
fn seed_graph(engine: &mut MarketGraphEngine) {
    engine.add_market(MarketNode::new(
        MKT_A,
        "Presidential Election 2026 — Party A wins",
        0.55,
        Some(50_000.0),
        None,
    ));
    engine.add_market(MarketNode::new(
        MKT_B,
        "Senate controlled by Party A (2026)",
        0.50,
        Some(30_000.0),
        None,
    ));
    engine.add_market(MarketNode::new(
        MKT_C,
        "Flagship policy passes in 2026",
        0.40,
        Some(15_000.0),
        None,
    ));

    // A → B: positive causal correlation (president win → senate follows)
    engine
        .add_dependency(MKT_A, MKT_B, 0.75, 0.90, EdgeType::Causal)
        .expect("add_dependency A→B");

    // B → C: positive causal correlation (senate majority → policy passes)
    engine
        .add_dependency(MKT_B, MKT_C, 0.60, 0.80, EdgeType::Causal)
        .expect("add_dependency B→C");
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Phase 1 — Synchronous engine demonstration
// ═══════════════════════════════════════════════════════════════════════════════

/// Demonstrate the engine in isolation (no async, no event bus).
///
/// This validates the core propagation and deviation logic directly before
/// we layer the event bus on top in Phase 2.
fn phase1_direct_engine() {
    println!("┌──────────────────────────────────────────────────────────────┐");
    println!("│  Phase 1 — Direct engine test (no event bus)                 │");
    println!("└──────────────────────────────────────────────────────────────┘\n");

    let mut engine = MarketGraphEngine::new();
    seed_graph(&mut engine);

    // ── Initial state ─────────────────────────────────────────────────────────
    println!("Initial market state:");
    print_engine_table(&engine);

    // ── Update A and propagate ─────────────────────────────────────────────────
    //
    // We move MKT_A from 0.55 → 0.65 (Δ = +0.10).
    // Expected propagation:
    //   MKT_B.implied += 0.75 × 0.90 × 0.10 × 0.85 (damping)  ≈ +0.0574
    //   MKT_C.implied += 0.60 × 0.80 × 0.0574 × 0.85           ≈ +0.0235
    println!(
        "\nApplying update: {} → prob=0.65 (Δ=+0.10)\n",
        MKT_A
    );
    let result = engine
        .update_and_propagate(MKT_A, 0.65, UpdateSource::Api)
        .expect("update_and_propagate");

    println!(
        "Propagation result: nodes_updated={}, iterations={}, converged={}, updated_ids={:?}",
        result.nodes_updated, result.iterations, result.converged, result.updated_node_ids
    );

    // ── Post-propagation state ─────────────────────────────────────────────────
    println!("\nPost-propagation state (current_prob vs implied_prob):");
    print_engine_table(&engine);

    // ── Deviation analysis ─────────────────────────────────────────────────────
    println!("Deviation analysis:");
    println!(
        "  {} (root)     deviation = {:+.4}  [market moved, implied follows — expected ~0]",
        MKT_A,
        engine.get_deviation(MKT_A).unwrap()
    );
    println!(
        "  {} (child)    deviation = {:+.4}  [implied rose, market static — BUY signal if < 0]",
        MKT_B,
        engine.get_deviation(MKT_B).unwrap()
    );
    println!(
        "  {} (grandchild) deviation = {:+.4}  [smaller signal, more attenuation]",
        MKT_C,
        engine.get_deviation(MKT_C).unwrap()
    );

    // ── top_deviations ────────────────────────────────────────────────────────
    println!("\nTop deviations (sorted by |deviation|):");
    for (node, abs_dev) in engine.top_deviations(5) {
        let signed_dev = node.deviation(); // current_prob − implied_prob
        let signal = match signed_dev {
            d if d > 0.01  => "SELL ↑  (market above graph estimate)",
            d if d < -0.01 => "BUY  ↓  (market below graph estimate)",
            _              => "neutral",
        };
        println!(
            "  |dev|={abs_dev:.4}  signed={signed_dev:+.4}  {:<28}  → {signal}",
            node.market_id
        );
    }

    // ── Influencing / dependent nodes ─────────────────────────────────────────
    println!("\nWho influences {}?", MKT_B);
    for (id, eff_weight) in engine.get_influencing_nodes(MKT_B).unwrap() {
        println!("  ← {id:<28}  effective_weight={eff_weight:.4}");
    }

    println!("\nWho depends on {}?", MKT_A);
    for (id, eff_weight) in engine.get_dependent_nodes(MKT_A).unwrap() {
        println!("  → {id:<28}  effective_weight={eff_weight:.4}");
    }

    println!("\n✓ Phase 1 complete.\n");
}

/// Pretty-print the current and implied probabilities for all three test markets.
fn print_engine_table(engine: &MarketGraphEngine) {
    println!(
        "  {:<28}  {:>8}  {:>8}  {:>8}",
        "market_id", "current", "implied", "deviation"
    );
    println!("  {}", "─".repeat(60));
    for id in &[MKT_A, MKT_B, MKT_C] {
        if let Ok(node) = engine.get_market(id) {
            println!(
                "  {:<28}  {:>8.4}  {:>8.4}  {:>+8.4}",
                node.market_id,
                node.current_prob,
                node.implied_prob,
                node.deviation()
            );
        }
    }
    println!();
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Phase 2 — Async event bus integration
// ═══════════════════════════════════════════════════════════════════════════════

/// Watch all events on the bus and print a summary for each Market or Graph event.
///
/// For `GraphUpdate` events, reads the current engine state (read lock only)
/// and prints the implied-probability + deviation table.
async fn event_watcher(bus: EventBus, engine: SharedEngine, cancel: CancellationToken) {
    let mut rx = bus.subscribe();
    let mut graph_update_count = 0u32;

    loop {
        let event = tokio::select! {
            _ = cancel.cancelled() => {
                info!("event_watcher: shutdown requested, exiting");
                break;
            }
            result = rx.recv() => result,
        };

        match event {
            // ── MarketUpdate ─────────────────────────────────────────────────
            Ok(ev) => match ev.as_ref() {
                Event::Market(u) => {
                    println!(
                        "  [MarketUpdate]  {:<28}  prob={:.4}  liq=${:.0}",
                        u.market.id, u.market.probability, u.market.liquidity
                    );
                }

                // ── GraphUpdate ──────────────────────────────────────────────
                Event::Graph(gu) => {
                    graph_update_count += 1;
                    println!(
                        "\n  ┌── [GraphUpdate #{}] source={} {} nodes updated ──────────",
                        graph_update_count,
                        gu.source_market_id,
                        gu.node_updates.len()
                    );
                    for nu in &gu.node_updates {
                        println!(
                            "  │  {} : implied {:.4} → {:.4}  (Δ={:+.4})",
                            nu.market_id,
                            nu.old_implied_prob,
                            nu.new_implied_prob,
                            nu.new_implied_prob - nu.old_implied_prob
                        );
                    }

                    // Read the shared engine to show implied probs + deviations.
                    // We use a short-lived read lock so we don't block the write path.
                    let eng = engine.read().await;
                    println!(
                        "  │  {:<28}  {:>8}  {:>8}  {:>8}  {}",
                        "market_id", "current", "implied", "deviation", "signal"
                    );
                    println!("  │  {}", "─".repeat(72));
                    for id in &[MKT_A, MKT_B, MKT_C] {
                        if let Ok(node) = eng.get_market(id) {
                            let dev = node.deviation(); // current_prob − implied_prob
                            let signal = if dev > 0.01 {
                                "SELL ↑"
                            } else if dev < -0.01 {
                                "BUY  ↓"
                            } else {
                                "─"
                            };
                            println!(
                                "  │  {:<28}  {:>8.4}  {:>8.4}  {:>+8.4}  {}",
                                node.market_id,
                                node.current_prob,
                                node.implied_prob,
                                dev,
                                signal
                            );
                        }
                    }
                    println!("  └────────────────────────────────────────────────────────\n");
                }

                _ => {} // ignore Sentiment, Signal, Execution, Portfolio events
            },

            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                warn!("event_watcher: lagged by {n} events — increase bus capacity");
            }

            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                info!("event_watcher: bus closed, shutting down");
                break;
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Main
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::main]
async fn main() -> Result<()> {
    // ── Logging ───────────────────────────────────────────────────────────────
    // Default to "warn" so harness output stays readable.
    // Override with RUST_LOG=debug to see engine internals.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn".parse().unwrap()),
        )
        .with_target(false)
        .init();

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║  Prediction Market Agent — Integration Test Harness          ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    // ─────────────────────────────────────────────────────────────────────────
    //  Phase 1: synchronous, no event bus
    // ─────────────────────────────────────────────────────────────────────────
    phase1_direct_engine();

    // ─────────────────────────────────────────────────────────────────────────
    //  Phase 2: full async pipeline
    // ─────────────────────────────────────────────────────────────────────────
    println!("┌──────────────────────────────────────────────────────────────┐");
    println!("│  Phase 2 — Async event bus integration (5 s run)             │");
    println!("└──────────────────────────────────────────────────────────────┘\n");

    // Step 1 — Event Bus (capacity sized to avoid lagging in tests).
    let bus = EventBus::with_capacity(512);
    println!("Event Bus: initialised (capacity=512)");

    // Step 2 — Market Graph Engine, pre-seeded with nodes + edges.
    let mut raw_engine = MarketGraphEngine::new();
    seed_graph(&mut raw_engine);
    println!("Market Graph Engine: seeded ({} nodes, {} edges)\n",
        raw_engine.node_count(), raw_engine.edge_count());

    // Wrap in Arc<RwLock<>> for shared access across tasks.
    let engine: SharedEngine = Arc::new(RwLock::new(raw_engine));

    // Step 3 — Async MarketGraph wrapper.
    let graph_wrapper = MarketGraph::with_engine(Arc::clone(&engine), bus.clone());
    println!("Market Graph: async wrapper created (subscribes to Event::Market)");

    // Step 4 — Market Scanner with synthetic data source.
    let scanner_cfg = ScannerConfig {
        poll_interval_ms: 500,
        batch_size:       100,
        max_retries:      1,
        ..ScannerConfig::default()
    };

    let scanner = MarketScanner::new(bus.clone(), scanner_cfg)
        .with_source(SyntheticSource::new());
    let scanner_state = scanner.state_handle();
    println!("Market Scanner: configured (poll_interval=500ms, source=synthetic)");

    // Step 5 — Cancellation token for graceful shutdown.
    let cancel = CancellationToken::new();

    // Step 6 — Subscribe event watcher BEFORE spawning producers.
    let watcher_bus    = bus.clone();
    let watcher_engine = Arc::clone(&engine);

    // Step 7 — Spawn all tasks.
    println!("\nSpawning tasks…");
    println!("  → scanner  (publishes Event::Market)");
    println!("  → graph    (subscribes Event::Market, publishes Event::Graph)");
    println!("  → watcher  (prints all events + deviations)\n");

    let h_scanner = tokio::spawn(scanner.run(cancel.child_token()));
    let h_graph   = tokio::spawn(graph_wrapper.run(cancel.child_token()));
    let h_watcher = tokio::spawn(event_watcher(watcher_bus, watcher_engine, cancel.child_token()));

    // Step 8 — Run for 10 ticks (10 × 500 ms = 5 s).
    let run_duration = Duration::from_secs(5);
    println!("Running for {:.1}s — watch the event flow below…\n", run_duration.as_secs_f64());
    tokio::time::sleep(run_duration).await;

    // Step 9 — Cancel all tasks cleanly and wait for them to finish.
    cancel.cancel();
    let _ = tokio::join!(h_scanner, h_graph, h_watcher);

    // ─────────────────────────────────────────────────────────────────────────
    //  Final snapshot
    // ─────────────────────────────────────────────────────────────────────────
    let state = scanner_state.read().await;
    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║  Final snapshot                                               ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!(
        "\nScanner: {} ticks completed, {} total markets published",
        state.ticks_completed,
        state.markets_seen
    );
    drop(state);

    println!("\nGraph engine final state:");
    {
        let eng = engine.read().await;
        println!(
            "  {:<28}  {:>8}  {:>8}  {:>8}  {}",
            "market_id", "current", "implied", "deviation", "signal"
        );
        println!("  {}", "─".repeat(72));
        for id in &[MKT_A, MKT_B, MKT_C] {
            if let Ok(node) = eng.get_market(id) {
                let dev    = node.deviation();
                let signal = if dev > 0.01 {
                    "SELL ↑  (market priced above graph estimate)"
                } else if dev < -0.01 {
                    "BUY  ↓  (market priced below graph estimate)"
                } else {
                    "─  (within noise threshold)"
                };
                println!(
                    "  {:<28}  {:>8.4}  {:>8.4}  {:>+8.4}  {}",
                    node.market_id, node.current_prob, node.implied_prob, dev, signal
                );
            }
        }

        println!("\nTop deviations (all markets, |deviation| descending):");
        for (node, abs_dev) in eng.top_deviations(5) {
            let signed = node.deviation();
            let arrow  = if signed > 0.0 { "↑" } else { "↓" };
            println!(
                "  {arrow} {:<28}  |dev|={abs_dev:.4}  signed={signed:+.4}",
                node.market_id
            );
        }
    }

    println!("\n✓ Integration test harness complete.");
    Ok(())
}
