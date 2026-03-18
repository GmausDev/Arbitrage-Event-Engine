// crates/meta_strategy/tests/meta_strategy.rs
//
// Integration tests for MetaStrategyEngine.
//
// Pattern: subscribe BEFORE spawning the engine, publish Signal/Shock events
// to the bus, wait for the engine to process them, then assert on the
// MetaSignal(s) collected from the bus.

use std::{sync::Arc, time::Duration};

use chrono::Utc;
use common::{
    Event, EventBus, InformationShock, MetaSignal, ShockDirection, ShockSource, TradeDirection,
    TradeSignal,
};
use meta_strategy::{MetaStrategyConfig, MetaStrategyEngine, MetaStrategyState};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn test_config() -> MetaStrategyConfig {
    MetaStrategyConfig {
        min_combined_confidence: 0.30,
        min_expected_edge:       0.005,
        shock_boost_factor:      0.25,
        shock_freshness_secs:    300,
        signal_ttl_secs:         60,
        min_strategies:          1,
    }
}

/// Build a `TradeSignal` with the given fields and source.
fn trade_signal(
    market_id: &str,
    direction: TradeDirection,
    confidence: f64,
    ev: f64,
    source: &str,
) -> Event {
    Event::Signal(TradeSignal {
        market_id:         market_id.to_string(),
        direction,
        expected_value:    ev,
        position_fraction: 0.05,
        posterior_prob:    0.70,
        market_prob:       0.50,
        confidence,
        timestamp:         Utc::now(),
        source:            source.to_string(),
    })
}

fn shock_event(market_id: &str, magnitude: f64) -> Event {
    Event::Shock(InformationShock {
        market_id:  market_id.to_string(),
        magnitude,
        direction:  ShockDirection::Up,
        source:     ShockSource::Volatility,
        z_score:    3.0,
        timestamp:  Utc::now(),
    })
}

async fn wait_for_events(state: &Arc<tokio::sync::RwLock<MetaStrategyState>>, expected: u64) {
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

/// Collect all `Event::MetaSignal` messages from `rx` within a short window.
async fn collect_meta_signals(
    rx: &mut broadcast::Receiver<Arc<Event>>,
    window: Duration,
) -> Vec<MetaSignal> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(ev)) => {
                if let Event::MetaSignal(ms) = ev.as_ref() {
                    out.push(ms.clone());
                }
            }
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            _ => break,
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A single strong Buy signal should immediately produce a MetaSignal(Buy).
#[tokio::test]
async fn single_signal_emits_meta_signal() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let engine = MetaStrategyEngine::new(test_config(), bus.clone());
    let state  = engine.state();
    tokio::spawn(engine.run(cancel.child_token()));

    bus.publish(trade_signal("M", TradeDirection::Buy, 0.80, 0.10, "bayesian_edge_agent")).ok();
    wait_for_events(&state, 1).await;

    let signals = collect_meta_signals(&mut verify_rx, Duration::from_millis(200)).await;
    let relevant: Vec<_> = signals.iter().filter(|s| s.market_id == "M").collect();

    assert!(!relevant.is_empty(), "expected a MetaSignal for market M");
    assert_eq!(relevant[0].direction, TradeDirection::Buy);
    assert!(relevant[0].confidence > 0.0);
    assert!(relevant[0].expected_edge > 0.0);
    cancel.cancel();
}

/// When Buy signals dominate by weight, the meta direction should be Buy.
#[tokio::test]
async fn buy_wins_over_conflicting_sell() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let engine = MetaStrategyEngine::new(test_config(), bus.clone());
    let state  = engine.state();
    tokio::spawn(engine.run(cancel.child_token()));

    // Buy: weight = 0.80 × 0.12 = 0.096
    bus.publish(trade_signal("M", TradeDirection::Buy,  0.80, 0.12, "bayesian_edge_agent")).ok();
    // Sell: weight = 0.40 × 0.06 = 0.024  (Buy weight 4× larger)
    bus.publish(trade_signal("M", TradeDirection::Sell, 0.40, 0.06, "temporal_agent")).ok();
    wait_for_events(&state, 2).await;

    let signals = collect_meta_signals(&mut verify_rx, Duration::from_millis(200)).await;
    let relevant: Vec<_> = signals.iter().filter(|s| s.market_id == "M").collect();

    assert!(!relevant.is_empty(), "expected at least one MetaSignal");
    // The last MetaSignal for M should reflect Buy after both signals arrive.
    let last = relevant.last().unwrap();
    assert_eq!(last.direction, TradeDirection::Buy);
    assert!(last.confidence > 0.30);
    cancel.cancel();
}

/// When Sell signals dominate by weight, the meta direction should be Sell.
#[tokio::test]
async fn sell_wins_over_conflicting_buy() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let engine = MetaStrategyEngine::new(test_config(), bus.clone());
    let state  = engine.state();
    tokio::spawn(engine.run(cancel.child_token()));

    bus.publish(trade_signal("M", TradeDirection::Buy,  0.30, 0.05, "graph_arb_agent")).ok();
    bus.publish(trade_signal("M", TradeDirection::Sell, 0.90, 0.20, "signal_agent")).ok();
    wait_for_events(&state, 2).await;

    let signals = collect_meta_signals(&mut verify_rx, Duration::from_millis(200)).await;
    let relevant: Vec<_> = signals.iter().filter(|s| s.market_id == "M").collect();

    assert!(!relevant.is_empty());
    let last = relevant.last().unwrap();
    assert_eq!(last.direction, TradeDirection::Sell);
    cancel.cancel();
}

/// A fresh shock should boost combined_confidence enough to cross the threshold.
///
/// We use `min_strategies: 2` so neither signal fires in isolation, ensuring
/// the "no signal before shock" assertion is unambiguous.
#[tokio::test]
async fn shock_boosts_weak_signal_above_threshold() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    // min_strategies=2 prevents either signal from firing alone.
    let cfg = MetaStrategyConfig {
        min_strategies: 2,
        ..test_config()
    };
    let engine = MetaStrategyEngine::new(cfg, bus.clone());
    let state  = engine.state();
    tokio::spawn(engine.run(cancel.child_token()));

    // Two signals that give combined_confidence = 0.25 < 0.30.
    // Buy: 0.50 × 0.10 = 0.050,  Sell: 0.60 × 0.05 = 0.030
    // net = 0.020, total = 0.080, combined_confidence = 0.25
    bus.publish(trade_signal("M", TradeDirection::Buy,  0.50, 0.10, "strat_buy")).ok();
    bus.publish(trade_signal("M", TradeDirection::Sell, 0.60, 0.05, "strat_sell")).ok();
    wait_for_events(&state, 2).await;

    // Both signals processed; combined_confidence = 0.25 < 0.30 → no MetaSignal.
    let before = collect_meta_signals(&mut verify_rx, Duration::from_millis(100)).await;
    let before_m: Vec<_> = before.iter().filter(|s| s.market_id == "M").collect();
    assert!(
        before_m.is_empty(),
        "should not emit MetaSignal before shock: combined_confidence = 0.25 < 0.30"
    );

    // Inject shock with magnitude=1.0:
    // boosted = 0.25 × (1 + 0.25 × 1.0) = 0.3125 ≥ 0.30.
    // The boost is applied on the NEXT Signal for this market, so re-publish
    // one signal to trigger re-evaluation after the shock is cached.
    bus.publish(shock_event("M", 1.0)).ok();
    bus.publish(trade_signal("M", TradeDirection::Buy, 0.50, 0.10, "strat_buy")).ok();
    wait_for_events(&state, 4).await;

    let after = collect_meta_signals(&mut verify_rx, Duration::from_millis(200)).await;
    let after_m: Vec<_> = after.iter().filter(|s| s.market_id == "M").collect();
    assert!(
        !after_m.is_empty(),
        "shock boost should push confidence to ≥ 0.30 and emit MetaSignal"
    );
    assert!(after_m[0].confidence >= 0.30);
    cancel.cancel();
}

/// Independent markets must not affect each other's signals.
#[tokio::test]
async fn independent_markets_do_not_interfere() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let engine = MetaStrategyEngine::new(test_config(), bus.clone());
    let state  = engine.state();
    tokio::spawn(engine.run(cancel.child_token()));

    // Market A: strong Buy.
    bus.publish(trade_signal("A", TradeDirection::Buy, 0.90, 0.15, "strat_x")).ok();
    // Market B: strong Sell.
    bus.publish(trade_signal("B", TradeDirection::Sell, 0.85, 0.12, "strat_x")).ok();
    wait_for_events(&state, 2).await;

    let signals = collect_meta_signals(&mut verify_rx, Duration::from_millis(300)).await;

    let a_signals: Vec<_> = signals.iter().filter(|s| s.market_id == "A").collect();
    let b_signals: Vec<_> = signals.iter().filter(|s| s.market_id == "B").collect();

    assert!(!a_signals.is_empty(), "market A should produce a MetaSignal");
    assert!(!b_signals.is_empty(), "market B should produce a MetaSignal");
    assert_eq!(a_signals.last().unwrap().direction, TradeDirection::Buy);
    assert_eq!(b_signals.last().unwrap().direction, TradeDirection::Sell);
    cancel.cancel();
}

/// Event counters should accurately track signals processed and emitted.
#[tokio::test]
async fn event_counters_accurate() {
    let bus = EventBus::new();
    let cancel = CancellationToken::new();

    let engine = MetaStrategyEngine::new(test_config(), bus.clone());
    let state  = engine.state();
    tokio::spawn(engine.run(cancel.child_token()));

    // Publish 3 signals (two for A, one for B); each should produce a MetaSignal.
    bus.publish(trade_signal("A", TradeDirection::Buy, 0.80, 0.10, "strat_1")).ok();
    bus.publish(trade_signal("A", TradeDirection::Buy, 0.70, 0.08, "strat_2")).ok();
    bus.publish(trade_signal("B", TradeDirection::Sell, 0.90, 0.15, "strat_1")).ok();
    wait_for_events(&state, 3).await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    let st = state.read().await;
    assert_eq!(st.events_processed, 3);
    // Each signal triggers an evaluation; all three should pass the gates.
    assert_eq!(st.signals_emitted, 3);
    cancel.cancel();
}

/// MetaSignals for different markets should each carry the correct RAEV ordering.
/// We verify this by collecting all MetaSignals from the bus and checking that
/// the signal for HIGH has a higher confidence × expected_edge than MID and LOW.
#[tokio::test]
async fn meta_signals_have_correct_raev_ordering() {
    let bus = EventBus::new();
    let mut verify_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let engine = MetaStrategyEngine::new(test_config(), bus.clone());
    let state  = engine.state();
    tokio::spawn(engine.run(cancel.child_token()));

    // Publish signals for 3 different markets with different quality.
    // Market "HIGH": confidence=0.90, ev=0.15 → single-signal RAEV = 0.90 × 0.15 = 0.135
    // Market "MID":  confidence=0.60, ev=0.10 → single-signal RAEV = 0.60 × 0.10 = 0.060
    // Market "LOW":  confidence=0.40, ev=0.05 → single-signal RAEV = 0.40 × 0.05 = 0.020
    bus.publish(trade_signal("HIGH", TradeDirection::Buy, 0.90, 0.15, "s")).ok();
    bus.publish(trade_signal("MID",  TradeDirection::Buy, 0.60, 0.10, "s")).ok();
    bus.publish(trade_signal("LOW",  TradeDirection::Buy, 0.40, 0.05, "s")).ok();
    wait_for_events(&state, 3).await;

    let signals = collect_meta_signals(&mut verify_rx, Duration::from_millis(300)).await;

    let high = signals.iter().find(|s| s.market_id == "HIGH").expect("HIGH MetaSignal missing");
    let mid  = signals.iter().find(|s| s.market_id == "MID").expect("MID MetaSignal missing");
    let low  = signals.iter().find(|s| s.market_id == "LOW").expect("LOW MetaSignal missing");

    let raev = |ms: &MetaSignal| ms.confidence * ms.expected_edge;

    assert!(
        raev(high) > raev(mid),
        "HIGH RAEV ({}) should exceed MID RAEV ({})", raev(high), raev(mid)
    );
    assert!(
        raev(mid) > raev(low),
        "MID RAEV ({}) should exceed LOW RAEV ({})", raev(mid), raev(low)
    );
    cancel.cancel();
}

/// The engine must forward MetaSignal events to the bus so downstream
/// consumers (portfolio_optimizer, risk_engine) can receive them.
#[tokio::test]
async fn meta_signal_published_to_bus() {
    let bus = EventBus::new();
    let mut any_rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let engine = MetaStrategyEngine::new(test_config(), bus.clone());
    let state  = engine.state();
    tokio::spawn(engine.run(cancel.child_token()));

    bus.publish(trade_signal("Z", TradeDirection::Buy, 0.80, 0.10, "strat")).ok();
    wait_for_events(&state, 1).await;

    // Scan all bus events for a MetaSignal within a short window.
    let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
    let mut found = false;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() { break; }
        match tokio::time::timeout(remaining, any_rx.recv()).await {
            Ok(Ok(ev)) => {
                if matches!(ev.as_ref(), Event::MetaSignal(_)) {
                    found = true;
                    break;
                }
            }
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            _ => break,
        }
    }
    assert!(found, "Event::MetaSignal should appear on the bus");
    cancel.cancel();
}
