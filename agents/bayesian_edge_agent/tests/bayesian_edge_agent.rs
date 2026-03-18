// agents/bayesian_edge_agent/tests/bayesian_edge_agent.rs
//
// Integration tests for the Bayesian Edge Agent.
//
// Pattern: subscribe BEFORE spawning the agent, publish events, collect
// signals within a timeout window.

use std::{sync::Arc, time::Duration};

use bayesian_edge_agent::{BayesianEdgeAgent, BayesianEdgeConfig, BayesianEdgeState};
use chrono::Utc;
use common::{
    Event, EventBus, GraphUpdate, InformationShock, MarketNode, MarketUpdate, NodeProbUpdate,
    PosteriorUpdate, ShockDirection, ShockSource, TradeDirection, TradeSignal,
};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn test_config() -> BayesianEdgeConfig {
    BayesianEdgeConfig {
        min_confidence: 0.25,
        min_expected_value: 0.01,
        trading_cost: 0.003,
        max_position_fraction: 0.10,
        kelly_fraction: 0.25,
        shock_boost_factor: 0.5,
        shock_freshness_secs: 300,
        graph_damp_factor: 0.20,
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
            last_update: Utc::now(),
        },
    })
}

fn posterior_update(market_id: &str, posterior: f64, confidence: f64) -> Event {
    Event::Posterior(PosteriorUpdate {
        market_id: market_id.to_string(),
        prior_prob: 0.5,
        posterior_prob: posterior,
        market_prob: None,
        deviation: 0.0,
        confidence,
        timestamp: Utc::now(),
    })
}

fn graph_update_single(source: &str, market_id: &str, implied: f64) -> Event {
    Event::Graph(GraphUpdate {
        source_market_id: source.to_string(),
        node_updates: vec![NodeProbUpdate {
            market_id: market_id.to_string(),
            old_implied_prob: 0.5,
            new_implied_prob: implied,
        }],
    })
}

fn shock_event(market_id: &str, magnitude: f64) -> Event {
    Event::Shock(InformationShock {
        market_id: market_id.to_string(),
        magnitude,
        direction: ShockDirection::Up,
        source: ShockSource::News,
        z_score: 0.0,
        timestamp: Utc::now(),
    })
}

async fn wait_for_events(
    state: &Arc<tokio::sync::RwLock<BayesianEdgeState>>,
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
async fn no_signal_without_market_price() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let agent = BayesianEdgeAgent::new(test_config(), bus.clone());
    let state = agent.state();
    tokio::spawn(agent.run(cancel.child_token()));

    // Posterior only — no market price.
    bus.publish(posterior_update("M", 0.70, 0.80)).ok();
    wait_for_events(&state, 1).await;

    let signals = collect_signals(&mut verify_rx, Duration::from_millis(100)).await;
    assert!(
        signals.is_empty(),
        "posterior without market price must not emit a signal"
    );
    cancel.cancel();
}

#[tokio::test]
async fn buy_signal_on_large_positive_deviation() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let agent = BayesianEdgeAgent::new(test_config(), bus.clone());
    let state = agent.state();
    tokio::spawn(agent.run(cancel.child_token()));

    bus.publish(market_update("M", 0.50)).ok();
    wait_for_events(&state, 1).await;

    bus.publish(posterior_update("M", 0.70, 0.80)).ok();
    wait_for_events(&state, 2).await;

    let signals = collect_signals(&mut verify_rx, Duration::from_millis(200)).await;
    let relevant: Vec<_> = signals.iter().filter(|s| s.market_id == "M").collect();

    assert!(!relevant.is_empty(), "expected a Buy signal");
    let s = &relevant[0];
    assert_eq!(s.direction, TradeDirection::Buy);
    assert!((s.expected_value - (0.20 - 0.003)).abs() < 1e-9);
    assert!(s.confidence >= 0.25);
    cancel.cancel();
}

#[tokio::test]
async fn sell_signal_on_negative_deviation() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let agent = BayesianEdgeAgent::new(test_config(), bus.clone());
    let state = agent.state();
    tokio::spawn(agent.run(cancel.child_token()));

    bus.publish(market_update("M", 0.70)).ok();
    wait_for_events(&state, 1).await;
    bus.publish(posterior_update("M", 0.30, 0.80)).ok();
    wait_for_events(&state, 2).await;

    let signals = collect_signals(&mut verify_rx, Duration::from_millis(200)).await;
    let relevant: Vec<_> = signals.iter().filter(|s| s.market_id == "M").collect();
    assert!(!relevant.is_empty());
    assert_eq!(relevant[0].direction, TradeDirection::Sell);
    cancel.cancel();
}

#[tokio::test]
async fn low_confidence_suppressed_without_shock() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let agent = BayesianEdgeAgent::new(test_config(), bus.clone());
    let state = agent.state();
    tokio::spawn(agent.run(cancel.child_token()));

    bus.publish(market_update("M", 0.50)).ok();
    wait_for_events(&state, 1).await;
    // confidence=0.10 < min_confidence=0.25
    bus.publish(posterior_update("M", 0.70, 0.10)).ok();
    wait_for_events(&state, 2).await;

    let signals = collect_signals(&mut verify_rx, Duration::from_millis(100)).await;
    assert!(
        signals.is_empty(),
        "low confidence must suppress the signal"
    );
    cancel.cancel();
}

#[tokio::test]
async fn shock_boosts_confidence_above_threshold() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let agent = BayesianEdgeAgent::new(test_config(), bus.clone());
    let state = agent.state();
    tokio::spawn(agent.run(cancel.child_token()));

    bus.publish(market_update("M", 0.50)).ok();
    wait_for_events(&state, 1).await;

    // Shock first (magnitude=1.0): boost = 1 + 0.5*1.0 = 1.5
    // With base_confidence=0.20: boosted = 0.30 > 0.25 → signal allowed.
    bus.publish(shock_event("M", 1.0)).ok();
    wait_for_events(&state, 2).await;

    bus.publish(posterior_update("M", 0.70, 0.20)).ok();
    wait_for_events(&state, 3).await;

    let signals = collect_signals(&mut verify_rx, Duration::from_millis(200)).await;
    let relevant: Vec<_> = signals.iter().filter(|s| s.market_id == "M").collect();
    assert!(
        !relevant.is_empty(),
        "shock should boost confidence enough to emit a signal"
    );
    assert!(relevant[0].confidence >= 0.25);
    cancel.cancel();
}

#[tokio::test]
async fn graph_damp_reduces_effective_deviation() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let agent = BayesianEdgeAgent::new(test_config(), bus.clone());
    let state = agent.state();
    tokio::spawn(agent.run(cancel.child_token()));

    // Market at 0.50, posterior at 0.80, graph-implied at 0.55.
    // posterior_adj = 0.8*0.80 + 0.2*0.55 = 0.64+0.11 = 0.75
    // deviation = 0.75 - 0.50 = 0.25; EV = 0.25 - 0.003 = 0.247
    bus.publish(market_update("M", 0.50)).ok();
    wait_for_events(&state, 1).await;
    bus.publish(graph_update_single("src", "M", 0.55)).ok();
    wait_for_events(&state, 2).await;
    bus.publish(posterior_update("M", 0.80, 0.90)).ok();
    wait_for_events(&state, 3).await;

    let signals = collect_signals(&mut verify_rx, Duration::from_millis(200)).await;
    let relevant: Vec<_> = signals.iter().filter(|s| s.market_id == "M").collect();
    assert!(!relevant.is_empty());
    // EV must be damped: less than 0.30 - 0.003 = 0.297 (undamped case)
    assert!(relevant[0].expected_value < 0.297, "graph damping should reduce EV");
    assert!((relevant[0].expected_value - 0.247).abs() < 1e-9, "expected EV = 0.247");
    cancel.cancel();
}

#[tokio::test]
async fn signal_fires_regardless_of_event_order() {
    // Signal must be emitted whether posterior arrives before or after market price.
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let agent = BayesianEdgeAgent::new(test_config(), bus.clone());
    let state = agent.state();
    tokio::spawn(agent.run(cancel.child_token()));

    // Posterior first, then market price.
    bus.publish(posterior_update("M", 0.70, 0.80)).ok();
    wait_for_events(&state, 1).await;
    bus.publish(market_update("M", 0.50)).ok();
    wait_for_events(&state, 2).await;

    let signals = collect_signals(&mut verify_rx, Duration::from_millis(200)).await;
    let relevant: Vec<_> = signals.iter().filter(|s| s.market_id == "M").collect();
    assert!(!relevant.is_empty(), "signal must fire even when posterior arrives first");
    cancel.cancel();
}

#[tokio::test]
async fn multi_market_independent_signals() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let agent = BayesianEdgeAgent::new(test_config(), bus.clone());
    let state = agent.state();
    tokio::spawn(agent.run(cancel.child_token()));

    // A: Buy signal (posterior=0.75, market=0.50)
    // B: Sell signal (posterior=0.25, market=0.60)
    bus.publish(market_update("A", 0.50)).ok();
    bus.publish(market_update("B", 0.60)).ok();
    wait_for_events(&state, 2).await;

    bus.publish(posterior_update("A", 0.75, 0.80)).ok();
    bus.publish(posterior_update("B", 0.25, 0.80)).ok();
    wait_for_events(&state, 4).await;

    let signals = collect_signals(&mut verify_rx, Duration::from_millis(300)).await;

    let a: Vec<_> = signals.iter().filter(|s| s.market_id == "A").collect();
    let b: Vec<_> = signals.iter().filter(|s| s.market_id == "B").collect();

    assert!(!a.is_empty() && a[0].direction == TradeDirection::Buy);
    assert!(!b.is_empty() && b[0].direction == TradeDirection::Sell);
    cancel.cancel();
}
