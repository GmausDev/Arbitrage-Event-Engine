// crates/shock_detector/tests/shock_detector_tests.rs
//
// Integration tests for the Information Shock Detector.
//
// Tests cover:
//  1. No shock emitted for a small, routine price move
//  2. Shock emitted for a sudden large price spike
//  3. Shock emitted for a negative (downward) price crash
//  4. Sentiment-only shock above threshold
//  5. No sentiment shock below threshold
//  6. Multi-market sentiment propagation (one update → shocks on N markets)
//  7. Price + sentiment fusion raises magnitude (quantitatively asserted)
//  8. Engine state does not grow without bound across many updates
//  9. Opposing sentiment direction does not suppress shock detection

use std::collections::HashMap;
use std::time::Duration;

use common::{
    Event, EventBus, MarketNode, MarketUpdate, SentimentUpdate, ShockDirection,
};
use shock_detector::{InformationShockDetector, ShockDetectorConfig, SharedShockState};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_bus() -> EventBus {
    EventBus::new()
}

fn market_update(id: &str, prob: f64) -> Event {
    Event::Market(MarketUpdate {
        market: MarketNode {
            id: id.to_string(),
            probability: prob,
            liquidity: 1_000.0,
            last_update: chrono::Utc::now(),
        },
    })
}

fn sentiment_update(markets: &[&str], score: f64) -> Event {
    Event::Sentiment(SentimentUpdate {
        related_market_ids: markets.iter().map(|s| s.to_string()).collect(),
        headline: "test headline".to_string(),
        sentiment_score: score,
        timestamp: chrono::Utc::now(),
    })
}

/// Wait until `state.events_processed >= expected`, polling every 5 ms.
/// Panics on timeout.
async fn wait_for_events(state: &SharedShockState, expected: u64) {
    let deadline = Duration::from_secs(5);
    let start = tokio::time::Instant::now();
    loop {
        if state.read().await.events_processed >= expected {
            return;
        }
        assert!(
            start.elapsed() < deadline,
            "timed out waiting for {} events processed (got {})",
            expected,
            state.read().await.events_processed
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Send `n` alternating small price moves to build up a stable delta history,
/// then wait until the engine has processed all `n` events.
async fn prime_market(
    bus: &EventBus,
    state: &SharedShockState,
    market_id: &str,
    base_prob: f64,
    n: usize,
) {
    let before = state.read().await.events_processed;
    for i in 0..n {
        let prob = base_prob + (if i % 2 == 0 { 0.001 } else { -0.001 });
        bus.publish(market_update(market_id, prob)).ok();
    }
    wait_for_events(state, before + n as u64).await;
}

// ---------------------------------------------------------------------------
// Test 1: No shock for a routine small move
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_shock_for_small_routine_move() {
    let bus = make_bus();
    let mut verify_rx = bus.subscribe();

    let engine = InformationShockDetector::new(ShockDetectorConfig::default(), bus.clone()).unwrap();
    let state = engine.state();
    let cancel = CancellationToken::new();
    tokio::spawn(engine.run(cancel.child_token()));

    // Prime with 25 small alternating moves → large history variance.
    prime_market(&bus, &state, "ROUTINE", 0.50, 25).await;

    let before = state.read().await.events_processed;
    // Send a tiny move — the high variance in history means the z-score
    // for this 0.001 step is very small (well below 2.5 threshold).
    bus.publish(market_update("ROUTINE", 0.501)).ok();
    wait_for_events(&state, before + 1).await;

    cancel.cancel();

    // Drain any buffered events and check none are shocks for ROUTINE.
    let timeout = Duration::from_millis(100);
    let result = tokio::time::timeout(timeout, async {
        loop {
            match verify_rx.try_recv() {
                Ok(ev) => {
                    if let Event::Shock(s) = ev.as_ref() {
                        if s.market_id == "ROUTINE" {
                            return true;
                        }
                    }
                }
                Err(_) => return false,
            }
        }
    })
    .await;

    assert!(
        result.unwrap_or(false) == false,
        "no shock expected for a routine small move"
    );
}

// ---------------------------------------------------------------------------
// Test 2: Shock emitted for a sudden large price spike (upward)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shock_emitted_for_large_price_spike() {
    let bus = make_bus();
    let mut verify_rx = bus.subscribe();

    let engine = InformationShockDetector::new(ShockDetectorConfig::default(), bus.clone()).unwrap();
    let state = engine.state();
    let cancel = CancellationToken::new();
    tokio::spawn(engine.run(cancel.child_token()));

    // Prime with stable tiny moves so history baseline has small variance.
    prime_market(&bus, &state, "SPIKE", 0.30, 25).await;

    // Big jump — 0.30 → 0.70 should far exceed z-threshold.
    bus.publish(market_update("SPIKE", 0.70)).ok();

    let deadline = Duration::from_millis(1_000);
    let mut found = false;

    let _ = tokio::time::timeout(deadline, async {
        loop {
            match verify_rx.recv().await {
                Ok(ev) => {
                    if let Event::Shock(s) = ev.as_ref() {
                        if s.market_id == "SPIKE" {
                            found = true;
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
    assert!(found, "expected a shock on SPIKE after large price jump");
}

// ---------------------------------------------------------------------------
// Test 3: Shock direction is Down for a price crash
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shock_direction_down_for_price_crash() {
    let bus = make_bus();
    let mut verify_rx = bus.subscribe();

    let engine = InformationShockDetector::new(ShockDetectorConfig::default(), bus.clone()).unwrap();
    let state = engine.state();
    let cancel = CancellationToken::new();
    tokio::spawn(engine.run(cancel.child_token()));

    prime_market(&bus, &state, "CRASH", 0.80, 25).await;
    // Crash from 0.80 → 0.40.
    bus.publish(market_update("CRASH", 0.40)).ok();

    let deadline = Duration::from_millis(1_000);
    let mut direction: Option<ShockDirection> = None;

    let _ = tokio::time::timeout(deadline, async {
        loop {
            match verify_rx.recv().await {
                Ok(ev) => {
                    if let Event::Shock(s) = ev.as_ref() {
                        if s.market_id == "CRASH" {
                            direction = Some(s.direction);
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
    assert_eq!(
        direction,
        Some(ShockDirection::Down),
        "crash should produce a Down shock"
    );
}

// ---------------------------------------------------------------------------
// Test 4: Sentiment-only shock above threshold
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sentiment_shock_above_threshold() {
    let bus = make_bus();
    let mut verify_rx = bus.subscribe();

    let engine = InformationShockDetector::new(ShockDetectorConfig::default(), bus.clone()).unwrap();
    let cancel = CancellationToken::new();
    tokio::spawn(engine.run(cancel.child_token()));

    // No market updates — send only a strong positive sentiment.
    bus.publish(sentiment_update(&["SENT_MKT"], 0.85)).ok();

    let deadline = Duration::from_millis(1_000);
    let mut magnitude: Option<f64> = None;
    let mut direction: Option<ShockDirection> = None;

    let _ = tokio::time::timeout(deadline, async {
        loop {
            match verify_rx.recv().await {
                Ok(ev) => {
                    if let Event::Shock(s) = ev.as_ref() {
                        if s.market_id == "SENT_MKT" {
                            magnitude = Some(s.magnitude);
                            direction = Some(s.direction);
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
    let mag = magnitude.expect("expected a sentiment shock for SENT_MKT");
    assert_eq!(direction, Some(ShockDirection::Up));
    assert!((mag - 0.85).abs() < 1e-9, "magnitude should equal |sentiment_score|");
}

// ---------------------------------------------------------------------------
// Test 5: No sentiment shock when score is below threshold
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_shock_for_weak_sentiment() {
    let bus = make_bus();
    let mut verify_rx = bus.subscribe();

    let engine = InformationShockDetector::new(ShockDetectorConfig::default(), bus.clone()).unwrap();
    let state = engine.state();
    let cancel = CancellationToken::new();
    tokio::spawn(engine.run(cancel.child_token()));

    let before = state.read().await.events_processed;
    // Score 0.1 is well below the default threshold (0.4).
    bus.publish(sentiment_update(&["WEAK_SENT"], 0.1)).ok();
    wait_for_events(&state, before + 1).await;

    cancel.cancel();

    // Drain and confirm no shock for WEAK_SENT.
    loop {
        match verify_rx.try_recv() {
            Ok(ev) => {
                if let Event::Shock(s) = ev.as_ref() {
                    assert_ne!(
                        s.market_id, "WEAK_SENT",
                        "no shock expected for weak sentiment score"
                    );
                }
            }
            Err(_) => break,
        }
    }
}

// ---------------------------------------------------------------------------
// Test 6: Multi-market sentiment propagation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn multi_market_sentiment_propagation() {
    let bus = make_bus();
    let mut verify_rx = bus.subscribe();

    let engine = InformationShockDetector::new(ShockDetectorConfig::default(), bus.clone()).unwrap();
    let cancel = CancellationToken::new();
    tokio::spawn(engine.run(cancel.child_token()));

    let markets = &["MKT_A", "MKT_B", "MKT_C"];
    // Strong negative sentiment for all three.
    bus.publish(sentiment_update(markets, -0.75)).ok();

    let deadline = Duration::from_millis(1_000);
    let mut received: HashMap<String, ShockDirection> = HashMap::new();

    let _ = tokio::time::timeout(deadline, async {
        loop {
            match verify_rx.recv().await {
                Ok(ev) => {
                    if let Event::Shock(s) = ev.as_ref() {
                        if markets.contains(&s.market_id.as_str()) {
                            received.insert(s.market_id.clone(), s.direction);
                        }
                    }
                }
                Err(_) => break,
            }
            if received.len() == markets.len() {
                break;
            }
        }
    })
    .await;

    cancel.cancel();
    assert_eq!(
        received.len(),
        3,
        "expected shocks for all 3 markets, got {:?}",
        received.keys().collect::<Vec<_>>()
    );
    for (_, dir) in &received {
        assert_eq!(*dir, ShockDirection::Down, "negative sentiment → Down");
    }
}

// ---------------------------------------------------------------------------
// Test 7: Price + sentiment fusion raises magnitude (quantitative assertion)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn price_and_sentiment_fusion_raises_magnitude() {
    let bus = make_bus();
    let mut verify_rx = bus.subscribe();

    // price_weight = 0.5 so fusion is clearly visible.
    let config = ShockDetectorConfig {
        price_weight: 0.5,
        ..ShockDetectorConfig::default()
    };
    let engine = InformationShockDetector::new(config, bus.clone()).unwrap();
    let state = engine.state();
    let cancel = CancellationToken::new();
    tokio::spawn(engine.run(cancel.child_token()));

    // Prime baseline — tight history so the price shock has magnitude = 1.0.
    prime_market(&bus, &state, "FUSE", 0.50, 25).await;

    // Inject strong positive sentiment and wait for the engine to record it.
    let before = state.read().await.events_processed;
    bus.publish(sentiment_update(&["FUSE"], 0.95)).ok();
    wait_for_events(&state, before + 1).await;

    // Now trigger a price shock.
    bus.publish(market_update("FUSE", 0.80)).ok();

    let deadline = Duration::from_millis(1_000);
    let mut fused_magnitude: Option<f64> = None;

    let _ = tokio::time::timeout(deadline, async {
        loop {
            match verify_rx.recv().await {
                Ok(ev) => {
                    if let Event::Shock(s) = ev.as_ref() {
                        // Pick the first price shock (z_score > 0) for FUSE.
                        if s.market_id == "FUSE" && s.z_score.abs() > 0.0 {
                            fused_magnitude = Some(s.magnitude);
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
    let mag = fused_magnitude.expect("expected a fused price shock on FUSE");

    // With price_weight=0.5, price_magnitude=1.0 (capped), sentiment=0.95:
    //   fused = 0.5 * 1.0 + 0.5 * 0.95 = 0.975
    // Allow ±0.05 tolerance for floating point and z-scale variation.
    assert!(
        (mag - 0.975).abs() < 0.05,
        "fused magnitude should be ~0.975 (0.5×1.0 + 0.5×0.95), got {mag}"
    );
}

// ---------------------------------------------------------------------------
// Test 8: Engine state does not grow without bound
// ---------------------------------------------------------------------------

#[tokio::test]
async fn engine_state_bounded_on_repeated_updates() {
    let bus = make_bus();

    let engine = InformationShockDetector::new(ShockDetectorConfig::default(), bus.clone()).unwrap();
    let state = engine.state();

    let cancel = CancellationToken::new();
    tokio::spawn(engine.run(cancel.child_token()));

    // Send 100 updates for the same two markets.
    let n: u64 = 100;
    for i in 0..n {
        let prob = 0.5 + (i % 10) as f64 * 0.001;
        bus.publish(market_update("BOUNDED_A", prob)).ok();
        bus.publish(market_update("BOUNDED_B", 1.0 - prob)).ok();
    }

    // Wait until all 200 events are processed before inspecting state.
    wait_for_events(&state, n * 2).await;
    cancel.cancel();

    let snap = state.read().await;
    // State should contain exactly the 2 distinct market IDs, not 200+ entries.
    assert_eq!(
        snap.markets.len(),
        2,
        "market state map should have exactly 2 entries, got {}",
        snap.markets.len()
    );
    // The rolling window per market must not grow past config.rolling_window.
    let window = ShockDetectorConfig::default().rolling_window;
    for (id, ms) in &snap.markets {
        assert!(
            ms.delta_history.len() <= window,
            "market {id} delta_history grew to {} (max {window})",
            ms.delta_history.len()
        );
    }
}

// ---------------------------------------------------------------------------
// Test 9: Opposing sentiment direction still triggers a shock
//         (fusion uses |sentiment| so direction mismatch does not suppress)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn opposing_sentiment_does_not_suppress_price_shock() {
    let bus = make_bus();
    let mut verify_rx = bus.subscribe();

    let config = ShockDetectorConfig {
        price_weight: 0.5,
        ..ShockDetectorConfig::default()
    };
    let engine = InformationShockDetector::new(config, bus.clone()).unwrap();
    let state = engine.state();
    let cancel = CancellationToken::new();
    tokio::spawn(engine.run(cancel.child_token()));

    // Prime baseline.
    prime_market(&bus, &state, "OPPOSE", 0.50, 25).await;

    // Strongly *negative* sentiment, then a *positive* price shock.
    let before = state.read().await.events_processed;
    bus.publish(sentiment_update(&["OPPOSE"], -0.9)).ok();
    wait_for_events(&state, before + 1).await;

    bus.publish(market_update("OPPOSE", 0.80)).ok();

    let deadline = Duration::from_millis(1_000);
    let mut found_shock: Option<(ShockDirection, f64)> = None;

    let _ = tokio::time::timeout(deadline, async {
        loop {
            match verify_rx.recv().await {
                Ok(ev) => {
                    if let Event::Shock(s) = ev.as_ref() {
                        if s.market_id == "OPPOSE" && s.z_score.abs() > 0.0 {
                            found_shock = Some((s.direction, s.magnitude));
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
    let (dir, mag) = found_shock
        .expect("expected a price shock on OPPOSE even with opposing sentiment");

    // Direction is taken from the price move (Up), not the sentiment (Down).
    assert_eq!(dir, ShockDirection::Up, "direction must reflect price move");

    // Magnitude is fused: |sentiment| still contributes even when direction
    // opposes the price move.  With price_weight=0.5 and sentiment_mag=0.9:
    //   fused ≈ 0.5 * 1.0 + 0.5 * 0.9 = 0.95
    assert!(
        mag > 0.0 && mag <= 1.0,
        "fused magnitude {mag} must be in (0, 1]"
    );
    // Specifically document the design: opposing sentiment increases, not decreases, magnitude.
    assert!(
        mag > 0.8,
        "opposing sentiment should still boost magnitude via |score| fusion, got {mag}"
    );
}
