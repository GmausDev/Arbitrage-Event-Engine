// agents/graph_arb_agent/tests/graph_arb_agent.rs
//
// Integration tests for the Graph Arbitrage Agent.
//
// Pattern: subscribe BEFORE spawning the agent to avoid race conditions,
// then publish events and collect signals within a timeout window.

use std::{sync::Arc, time::Duration};

use common::{
    Event, EventBus, GraphUpdate, MarketNode, MarketUpdate, NodeProbUpdate, TradeDirection,
    TradeSignal,
};
use graph_arb_agent::{GraphArbAgent, GraphArbConfig, GraphArbState};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn test_config() -> GraphArbConfig {
    GraphArbConfig {
        min_edge_threshold: 0.04,
        trading_cost: 0.003,
        max_position_fraction: 0.10,
        kelly_fraction: 0.25,
        confidence_scale: 0.20,
        min_market_price: 0.05,
        max_market_price: 0.95,
    }
}

fn market_update(market_id: &str, prob: f64) -> Event {
    Event::Market(MarketUpdate {
        market: MarketNode {
            id: market_id.to_string(),
            probability: prob,
            liquidity: 10_000.0,
            last_update: chrono::Utc::now(),
        },
    })
}

fn graph_update(source: &str, nodes: Vec<(&str, f64, f64)>) -> Event {
    Event::Graph(GraphUpdate {
        source_market_id: source.to_string(),
        node_updates: nodes
            .into_iter()
            .map(|(id, old, new)| NodeProbUpdate {
                market_id: id.to_string(),
                old_implied_prob: old,
                new_implied_prob: new,
            })
            .collect(),
    })
}

async fn wait_for_events(
    state: &Arc<tokio::sync::RwLock<GraphArbState>>,
    expected: u64,
) {
    let deadline = Duration::from_secs(5);
    let start = tokio::time::Instant::now();
    loop {
        if state.read().await.events_processed >= expected {
            return;
        }
        assert!(
            start.elapsed() < deadline,
            "timed out waiting for {expected} events"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Collect all `Event::Signal` messages from `rx` within a short window.
async fn collect_signals(
    rx: &mut broadcast::Receiver<Arc<Event>>,
    window: Duration,
) -> Vec<TradeSignal> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(ev)) => {
                if let Event::Signal(s) = ev.as_ref() {
                    out.push(s.clone());
                }
            }
            _ => break,
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_signal_without_graph_update() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let agent = GraphArbAgent::new(test_config(), bus.clone());
    let state = agent.state();
    tokio::spawn(agent.run(cancel.child_token()));

    // Market update only — no graph-implied prob yet.
    bus.publish(market_update("M", 0.50)).ok();
    wait_for_events(&state, 1).await;

    let signals = collect_signals(&mut verify_rx, Duration::from_millis(100)).await;
    assert!(
        signals.is_empty(),
        "market-only update must not produce a signal"
    );
    cancel.cancel();
}

#[tokio::test]
async fn no_signal_without_market_update() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let agent = GraphArbAgent::new(test_config(), bus.clone());
    let state = agent.state();
    tokio::spawn(agent.run(cancel.child_token()));

    // Graph update only — no market price cached yet.
    bus.publish(graph_update("src", vec![("M", 0.50, 0.70)])).ok();
    wait_for_events(&state, 1).await;

    let signals = collect_signals(&mut verify_rx, Duration::from_millis(100)).await;
    assert!(
        signals.is_empty(),
        "graph-only update must not produce a signal"
    );
    cancel.cancel();
}

#[tokio::test]
async fn buy_signal_when_implied_above_market() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let agent = GraphArbAgent::new(test_config(), bus.clone());
    let state = agent.state();
    tokio::spawn(agent.run(cancel.child_token()));

    // Set market price, then graph update with implied > market.
    bus.publish(market_update("M", 0.50)).ok();
    wait_for_events(&state, 1).await;

    bus.publish(graph_update("src", vec![("M", 0.50, 0.70)])).ok();
    wait_for_events(&state, 2).await;

    let signals = collect_signals(&mut verify_rx, Duration::from_millis(200)).await;
    let graph_signals: Vec<_> = signals
        .iter()
        .filter(|s| s.market_id == "M")
        .collect();

    assert!(!graph_signals.is_empty(), "expected a Buy signal");
    let s = &graph_signals[0];
    assert_eq!(s.direction, TradeDirection::Buy);
    assert!((s.expected_value - (0.20 - 0.003)).abs() < 1e-9);
    assert!(s.position_fraction > 0.0 && s.position_fraction <= 0.10);
    assert!(s.confidence > 0.0 && s.confidence <= 1.0);
    cancel.cancel();
}

#[tokio::test]
async fn sell_signal_when_implied_below_market() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let agent = GraphArbAgent::new(test_config(), bus.clone());
    let state = agent.state();
    tokio::spawn(agent.run(cancel.child_token()));

    bus.publish(market_update("M", 0.60)).ok();
    wait_for_events(&state, 1).await;

    bus.publish(graph_update("src", vec![("M", 0.60, 0.30)])).ok();
    wait_for_events(&state, 2).await;

    let signals = collect_signals(&mut verify_rx, Duration::from_millis(200)).await;
    let graph_signals: Vec<_> = signals
        .iter()
        .filter(|s| s.market_id == "M")
        .collect();

    assert!(!graph_signals.is_empty(), "expected a Sell signal");
    assert_eq!(graph_signals[0].direction, TradeDirection::Sell);
    cancel.cancel();
}

#[tokio::test]
async fn no_signal_below_edge_threshold() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let agent = GraphArbAgent::new(test_config(), bus.clone());
    let state = agent.state();
    tokio::spawn(agent.run(cancel.child_token()));

    // delta = 0.02 < threshold = 0.04
    bus.publish(market_update("M", 0.50)).ok();
    wait_for_events(&state, 1).await;
    bus.publish(graph_update("src", vec![("M", 0.50, 0.52)])).ok();
    wait_for_events(&state, 2).await;

    let signals = collect_signals(&mut verify_rx, Duration::from_millis(100)).await;
    assert!(
        signals.is_empty(),
        "small edge must not produce a signal"
    );
    cancel.cancel();
}

#[tokio::test]
async fn multi_market_graph_update_produces_multiple_signals() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let agent = GraphArbAgent::new(test_config(), bus.clone());
    let state = agent.state();
    tokio::spawn(agent.run(cancel.child_token()));

    // Seed all three markets with prices.
    bus.publish(market_update("A", 0.50)).ok();
    bus.publish(market_update("B", 0.50)).ok();
    bus.publish(market_update("C", 0.50)).ok();
    wait_for_events(&state, 3).await;

    // One GraphUpdate with three nodes that all have large edges.
    bus.publish(graph_update(
        "root",
        vec![
            ("A", 0.50, 0.70),
            ("B", 0.50, 0.20),
            ("C", 0.50, 0.80),
        ],
    ))
    .ok();
    wait_for_events(&state, 4).await;

    let signals = collect_signals(&mut verify_rx, Duration::from_millis(300)).await;
    let relevant: Vec<_> = signals
        .iter()
        .filter(|s| ["A", "B", "C"].contains(&s.market_id.as_str()))
        .collect();

    assert_eq!(relevant.len(), 3, "expected one signal per updated node");

    let ids: std::collections::HashSet<_> = relevant.iter().map(|s| s.market_id.as_str()).collect();
    assert!(ids.contains("A") && ids.contains("B") && ids.contains("C"));
    cancel.cancel();
}

#[tokio::test]
async fn signal_state_counters_accurate() {
    let bus = EventBus::new();
    let cancel = CancellationToken::new();

    let agent = GraphArbAgent::new(test_config(), bus.clone());
    let state = agent.state();
    tokio::spawn(agent.run(cancel.child_token()));

    bus.publish(market_update("M", 0.50)).ok();
    wait_for_events(&state, 1).await;
    bus.publish(graph_update("src", vec![("M", 0.50, 0.80)])).ok();
    wait_for_events(&state, 2).await;

    // Allow signal emission to complete.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let st = state.read().await;
    assert_eq!(st.signals_emitted, 1);
    assert_eq!(st.events_processed, 2);
    cancel.cancel();
}
