// agents/temporal_agent/tests/temporal_agent.rs
//
// Integration tests for the Temporal Strategy Agent.
//
// Pattern: subscribe BEFORE spawning the agent, publish market updates to
// build price history, then send a breakout price and expect a trend signal.

use std::{sync::Arc, time::Duration};

use common::{Event, EventBus, MarketNode, MarketUpdate, TradeDirection, TradeSignal};
use temporal_agent::{TemporalAgent, TemporalConfig, TemporalState};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn test_config() -> TemporalConfig {
    TemporalConfig {
        history_window: 20,
        min_history: 5,
        trend_z_threshold: 1.5,
        trend_z_scale: 3.0,
        trading_cost: 0.003,
        max_position_fraction: 0.05,
        kelly_fraction: 0.25,
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

async fn wait_for_events(
    state: &Arc<tokio::sync::RwLock<TemporalState>>,
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

/// Prime a market with `n` stable prices around `base_prob` to build history.
/// Uses alternating ±`amp` offsets to ensure non-zero stddev.
async fn prime_history(
    bus: &EventBus,
    state: &Arc<tokio::sync::RwLock<TemporalState>>,
    market_id: &str,
    base_prob: f64,
    amp: f64,
    n: usize,
) {
    let before = state.read().await.events_processed;
    for i in 0..n {
        let price = base_prob + if i % 2 == 0 { amp } else { -amp };
        bus.publish(market_update(market_id, price)).ok();
    }
    wait_for_events(state, before + n as u64).await;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_signal_with_insufficient_history() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let agent = TemporalAgent::new(test_config(), bus.clone()).unwrap();
    let state = agent.state();
    tokio::spawn(agent.run(cancel.child_token()));

    // Publish only 3 updates — below min_history=5.
    for _ in 0..3 {
        bus.publish(market_update("M", 0.50)).ok();
    }
    wait_for_events(&state, 3).await;

    let signals = collect_signals(&mut verify_rx, Duration::from_millis(100)).await;
    assert!(
        signals.is_empty(),
        "fewer than min_history updates must not produce a signal"
    );
    cancel.cancel();
}

#[tokio::test]
async fn buy_signal_on_large_upward_breakout() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let agent = TemporalAgent::new(test_config(), bus.clone()).unwrap();
    let state = agent.state();
    tokio::spawn(agent.run(cancel.child_token()));

    // Build tight history around 0.50 (stddev≈0.001).
    prime_history(&bus, &state, "M", 0.50, 0.001, 15).await;

    // Breakout price: 0.75 — trend=0.25, z≈250 >> 1.5.
    bus.publish(market_update("M", 0.75)).ok();
    wait_for_events(&state, 16).await;

    let signals = collect_signals(&mut verify_rx, Duration::from_millis(300)).await;
    let relevant: Vec<_> = signals.iter().filter(|s| s.market_id == "M").collect();

    assert!(!relevant.is_empty(), "expected a Buy signal");
    let s = &relevant[0];
    assert_eq!(s.direction, TradeDirection::Buy);
    assert!(s.expected_value > 0.0);
    assert!(s.confidence > 0.0 && s.confidence <= 1.0);
    assert!(s.position_fraction > 0.0 && s.position_fraction <= 0.05);
    cancel.cancel();
}

#[tokio::test]
async fn sell_signal_on_large_downward_break() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let agent = TemporalAgent::new(test_config(), bus.clone()).unwrap();
    let state = agent.state();
    tokio::spawn(agent.run(cancel.child_token()));

    prime_history(&bus, &state, "M", 0.60, 0.001, 15).await;

    // Price crashes to 0.30 — strongly below mean.
    bus.publish(market_update("M", 0.30)).ok();
    wait_for_events(&state, 16).await;

    let signals = collect_signals(&mut verify_rx, Duration::from_millis(300)).await;
    let relevant: Vec<_> = signals.iter().filter(|s| s.market_id == "M").collect();

    assert!(!relevant.is_empty(), "expected a Sell signal");
    assert_eq!(relevant[0].direction, TradeDirection::Sell);
    cancel.cancel();
}

#[tokio::test]
async fn no_signal_on_stable_prices() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let cfg = TemporalConfig {
        // Tighter threshold so only true outliers fire.
        trend_z_threshold: 2.0,
        ..test_config()
    };
    let agent = TemporalAgent::new(cfg, bus.clone()).unwrap();
    let state = agent.state();
    tokio::spawn(agent.run(cancel.child_token()));

    // Publish prices that never deviate much from 0.50.
    // With amp=0.01 the stddev≈0.01; a price of 0.515 gives z≈1.5 < 2.0.
    prime_history(&bus, &state, "M", 0.50, 0.01, 20).await;

    let signals = collect_signals(&mut verify_rx, Duration::from_millis(150)).await;
    let relevant: Vec<_> = signals.iter().filter(|s| s.market_id == "M").collect();
    assert!(
        relevant.is_empty(),
        "routine prices must not produce a signal"
    );
    cancel.cancel();
}

#[tokio::test]
async fn independent_histories_per_market() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let agent = TemporalAgent::new(test_config(), bus.clone()).unwrap();
    let state = agent.state();
    tokio::spawn(agent.run(cancel.child_token()));

    // Prime two markets independently.
    prime_history(&bus, &state, "A", 0.40, 0.001, 15).await;
    prime_history(&bus, &state, "B", 0.60, 0.001, 15).await;

    // Breakout only on A.
    bus.publish(market_update("A", 0.70)).ok();
    wait_for_events(&state, 31).await;

    let signals = collect_signals(&mut verify_rx, Duration::from_millis(300)).await;

    let a_signals: Vec<_> = signals.iter().filter(|s| s.market_id == "A").collect();
    let b_signals: Vec<_> = signals.iter().filter(|s| s.market_id == "B").collect();

    assert!(!a_signals.is_empty(), "A should fire a signal");
    assert!(b_signals.is_empty(), "B has no breakout, should stay silent");
    cancel.cancel();
}

#[tokio::test]
async fn event_counters_accurate() {
    let bus = EventBus::new();
    let cancel = CancellationToken::new();

    let agent = TemporalAgent::new(test_config(), bus.clone()).unwrap();
    let state = agent.state();
    tokio::spawn(agent.run(cancel.child_token()));

    prime_history(&bus, &state, "M", 0.50, 0.001, 10).await;
    bus.publish(market_update("M", 0.80)).ok();
    wait_for_events(&state, 11).await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    let st = state.read().await;
    assert_eq!(st.events_processed, 11);
    assert_eq!(st.signals_emitted, 1);
    cancel.cancel();
}
