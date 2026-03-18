// crates/risk_engine/tests/risk_engine.rs
//
// Integration tests for RiskEngine.
//
// Each test spins up a real EventBus, optionally pre-seeds state, spawns the
// engine, publishes a TradeSignal, and asserts on the resulting ApprovedTrade
// (or absence thereof) collected from the bus.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use common::{Event, EventBus, TradeDirection, TradeSignal};
use risk_engine::{RiskConfig, RiskEngine};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn test_config() -> RiskConfig {
    RiskConfig::default()
    // defaults: max_position_fraction=0.05, max_total_exposure=0.40,
    //           max_cluster_exposure=0.15, min_expected_value=0.02, max_drawdown=0.20
}

fn make_signal(
    market_id: &str,
    ev: f64,
    position_fraction: f64,
    posterior: f64,
    market: f64,
) -> TradeSignal {
    TradeSignal {
        market_id:        market_id.to_string(),
        direction:        TradeDirection::Buy,
        expected_value:   ev,
        position_fraction,
        posterior_prob:   posterior,
        market_prob:      market,
        confidence:       0.80,
        timestamp:        Utc::now(),
        source:           String::new(),
    }
}

/// Subscribe BEFORE spawning so we never miss a fast approval.
fn spawn_engine(bus: &EventBus, config: RiskConfig) -> (RiskEngine, CancellationToken) {
    let engine = RiskEngine::new(config, bus.clone());
    let cancel = CancellationToken::new();
    (engine, cancel)
}

/// Drain the receiver and return the first `ApprovedTrade` received within
/// `timeout_ms`, or `None`.
///
/// Must be given a receiver that was subscribed *before* the events that
/// could trigger an approval were published.
async fn collect_approved(
    mut rx: tokio::sync::broadcast::Receiver<Arc<Event>>,
    timeout_ms: u64,
) -> Option<common::ApprovedTrade> {
    let result = timeout(Duration::from_millis(timeout_ms), async {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if let Event::ApprovedTrade(trade) = ev.as_ref() {
                        return Some(trade.clone());
                    }
                }
                _ => return None,
            }
        }
    })
    .await;
    result.unwrap_or(None)
}

// ---------------------------------------------------------------------------
// 1. EV filter rejects low EV signals
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ev_filter_rejects_low_ev() {
    let bus = EventBus::new();
    let rx  = bus.subscribe();

    let (engine, cancel) = spawn_engine(&bus, test_config());
    let c = cancel.clone();
    tokio::spawn(async move { engine.run(c).await });

    // EV = 0.01 < min_expected_value = 0.02 → should be rejected
    bus.publish(Event::Signal(make_signal("MARKET-A", 0.01, 0.05, 0.65, 0.50)))
        .unwrap();

    let trade = collect_approved(rx, 300).await;
    cancel.cancel();
    assert!(trade.is_none(), "signal with EV below threshold must not be approved");
}

// ---------------------------------------------------------------------------
// 2. max_position_fraction caps position
// ---------------------------------------------------------------------------

#[tokio::test]
async fn position_fraction_capped_at_max() {
    let bus = EventBus::new();
    let rx  = bus.subscribe();

    let (engine, cancel) = spawn_engine(&bus, test_config());
    let c = cancel.clone();
    tokio::spawn(async move { engine.run(c).await });

    // requested = 0.20, max = 0.05 → approved should be 0.05
    bus.publish(Event::Signal(make_signal("MARKET-B", 0.07, 0.20, 0.65, 0.50)))
        .unwrap();

    let trade = collect_approved(rx, 300).await;
    cancel.cancel();

    let trade = trade.expect("signal should be approved");
    assert!(
        trade.approved_fraction <= 0.05 + 1e-9,
        "approved_fraction ({}) must not exceed max_position_fraction (0.05)",
        trade.approved_fraction
    );
}

// ---------------------------------------------------------------------------
// 3. max_total_exposure prevents over-allocation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn total_exposure_full_rejects_signal() {
    let bus = EventBus::new();
    let rx  = bus.subscribe();

    let (engine, cancel) = spawn_engine(&bus, test_config());
    // Pre-seed: exposure already at max
    {
        let mut state = engine.state.write().await;
        state.total_exposure = 0.40;
    }
    let c = cancel.clone();
    tokio::spawn(async move { engine.run(c).await });

    bus.publish(Event::Signal(make_signal("MARKET-C", 0.07, 0.05, 0.65, 0.50)))
        .unwrap();

    let trade = collect_approved(rx, 300).await;
    cancel.cancel();
    assert!(trade.is_none(), "should reject when total exposure is full");
}

// ---------------------------------------------------------------------------
// 4. Cluster exposure cap works
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cluster_exposure_full_rejects_signal() {
    let bus = EventBus::new();
    let rx  = bus.subscribe();

    let (engine, cancel) = spawn_engine(&bus, test_config());
    // Pre-seed: "US" cluster already at max
    {
        let mut state = engine.state.write().await;
        state.cluster_exposure.insert("US".to_string(), 0.15);
    }
    let c = cancel.clone();
    tokio::spawn(async move { engine.run(c).await });

    // "US_ELECTION_HARRIS" → cluster = "US" → already full
    bus.publish(Event::Signal(make_signal(
        "US_ELECTION_HARRIS", 0.07, 0.05, 0.65, 0.50,
    )))
    .unwrap();

    let trade = collect_approved(rx, 300).await;
    cancel.cancel();
    assert!(trade.is_none(), "should reject when cluster exposure is full");
}

// ---------------------------------------------------------------------------
// 5. Drawdown protection stops trading
// ---------------------------------------------------------------------------

#[tokio::test]
async fn drawdown_protection_rejects_signal() {
    let bus = EventBus::new();
    let rx  = bus.subscribe();

    let (engine, cancel) = spawn_engine(&bus, test_config());
    // 25 % drawdown > 20 % max_drawdown
    {
        let mut state = engine.state.write().await;
        state.peak_equity    = 10_000.0;
        state.current_equity =  7_500.0;
    }
    let c = cancel.clone();
    tokio::spawn(async move { engine.run(c).await });

    bus.publish(Event::Signal(make_signal("MARKET-D", 0.07, 0.05, 0.65, 0.50)))
        .unwrap();

    let trade = collect_approved(rx, 300).await;
    cancel.cancel();
    assert!(trade.is_none(), "should reject when drawdown exceeds max_drawdown");
}

// ---------------------------------------------------------------------------
// 6. Signal accepted when all constraints satisfied
// ---------------------------------------------------------------------------

#[tokio::test]
async fn signal_accepted_all_constraints_satisfied() {
    let bus = EventBus::new();
    let rx  = bus.subscribe();

    let (engine, cancel) = spawn_engine(&bus, test_config());
    let c = cancel.clone();
    tokio::spawn(async move { engine.run(c).await });

    // market_id = "US_ELECTION_TRUMP", EV = 0.07, requested = 0.08
    // max_position_fraction = 0.05 → capped to 0.05
    bus.publish(Event::Signal(make_signal(
        "US_ELECTION_TRUMP", 0.07, 0.08, 0.65, 0.50,
    )))
    .unwrap();

    let trade = collect_approved(rx, 300).await;
    cancel.cancel();

    let trade = trade.expect("signal should be approved");
    assert_eq!(trade.market_id, "US_ELECTION_TRUMP");
    assert!(
        (trade.approved_fraction - 0.05).abs() < 1e-9,
        "approved_fraction should be 0.05 (capped from 0.08)"
    );
    assert!((trade.expected_value - 0.07).abs() < 1e-9);
}

// ---------------------------------------------------------------------------
// 7. Signal resized when total exposure nearly full
// ---------------------------------------------------------------------------

#[tokio::test]
async fn signal_resized_when_exposure_nearly_full() {
    let bus = EventBus::new();
    let rx  = bus.subscribe();

    let (engine, cancel) = spawn_engine(&bus, test_config());
    // 37 % used, max 40 % → only 3 % remaining
    {
        let mut state = engine.state.write().await;
        state.total_exposure = 0.37;
    }
    let c = cancel.clone();
    tokio::spawn(async move { engine.run(c).await });

    // Requested 0.05, but only 0.03 available → should approve 0.03
    bus.publish(Event::Signal(make_signal("MARKET-E", 0.07, 0.05, 0.65, 0.50)))
        .unwrap();

    let trade = collect_approved(rx, 300).await;
    cancel.cancel();

    let trade = trade.expect("signal should be approved at reduced size");
    assert!(
        (trade.approved_fraction - 0.03).abs() < 1e-9,
        "approved_fraction should be resized to 0.03, got {}",
        trade.approved_fraction
    );
}

// ---------------------------------------------------------------------------
// 8. Signal approved at exact min_expected_value boundary
// ---------------------------------------------------------------------------

#[tokio::test]
async fn signal_approved_at_min_ev_boundary() {
    let bus = EventBus::new();
    let rx  = bus.subscribe();

    let (engine, cancel) = spawn_engine(&bus, test_config());
    let c = cancel.clone();
    tokio::spawn(async move { engine.run(c).await });

    // EV exactly at min_expected_value = 0.02 → approved (≥, not >)
    bus.publish(Event::Signal(make_signal("MARKET-F", 0.02, 0.05, 0.55, 0.50)))
        .unwrap();

    let trade = collect_approved(rx, 300).await;
    cancel.cancel();
    assert!(trade.is_some(), "signal at exact min_expected_value must be approved");
}

// ---------------------------------------------------------------------------
// 9. Cluster resize when cluster nearly full
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cluster_resize_when_cluster_nearly_full() {
    let bus = EventBus::new();
    let rx  = bus.subscribe();

    let (engine, cancel) = spawn_engine(&bus, test_config());
    // "US" cluster at 0.13, max 0.15 → only 0.02 remaining
    {
        let mut state = engine.state.write().await;
        state.cluster_exposure.insert("US".to_string(), 0.13);
    }
    let c = cancel.clone();
    tokio::spawn(async move { engine.run(c).await });

    // Requested 0.05 for a "US_*" market — should be resized to 0.02
    bus.publish(Event::Signal(make_signal(
        "US_ELECTION_TRUMP", 0.07, 0.05, 0.65, 0.50,
    )))
    .unwrap();

    let trade = collect_approved(rx, 300).await;
    cancel.cancel();

    let trade = trade.expect("signal should be approved at reduced cluster size");
    assert!(
        (trade.approved_fraction - 0.02).abs() < 1e-9,
        "approved_fraction should be 0.02 (cluster cap), got {}",
        trade.approved_fraction
    );
}
