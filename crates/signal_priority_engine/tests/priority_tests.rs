// crates/signal_priority_engine/tests/priority_tests.rs

use std::time::Duration;

use chrono::Utc;
use common::{Event, EventBus, TradeDirection, TradeSignal};
use signal_priority_engine::{PriorityConfig, SignalPriorityEngine};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_signal(market_id: &str, ev: f64, confidence: f64) -> TradeSignal {
    TradeSignal {
        market_id:         market_id.to_string(),
        direction:         TradeDirection::Buy,
        expected_value:    ev,
        position_fraction: 0.05,
        posterior_prob:     0.6,
        market_prob:       0.5,
        confidence,
        timestamp:         Utc::now(),
        source:            "test".to_string(),
    }
}

fn default_config() -> PriorityConfig {
    PriorityConfig {
        batch_window_ms:     200,
        top_n:               3,
        fast_raev_threshold: 0.04,
        min_slow_ev:         0.01,
        slow_queue_capacity: 10,
    }
}

/// Spawn the engine, return a cancellation token.
fn spawn_engine(engine: SignalPriorityEngine) -> CancellationToken {
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move { engine.run(c).await });
    cancel
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// High EV (0.10) * high confidence (0.8) = 0.08 >= threshold 0.04 → FastSignal.
#[tokio::test]
async fn fast_signal_classification() {
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let engine = SignalPriorityEngine::new(default_config(), bus.clone()).unwrap();
    let cancel = spawn_engine(engine);

    // Give the engine a moment to start its event loop.
    tokio::task::yield_now().await;

    let signal = make_signal("mkt-fast", 0.10, 0.8);
    bus.publish(Event::Signal(signal)).unwrap();

    // Wait for the FastSignal event.
    let received = timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(ev) = rx.recv().await {
                if let Event::FastSignal(fs) = ev.as_ref() {
                    return fs.clone();
                }
            }
        }
    })
    .await
    .expect("timed out waiting for FastSignal");

    assert_eq!(received.signal.market_id, "mkt-fast");
    assert!((received.priority_score - 0.08).abs() < 1e-9);

    cancel.cancel();
}

/// Low score signal is buffered — no FastSignal is emitted.
/// The signal may eventually appear in a TopSignalsBatch (slow path) but must
/// never be promoted to FastSignal.
#[tokio::test]
async fn slow_signal_buffered() {
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let engine = SignalPriorityEngine::new(default_config(), bus.clone()).unwrap();
    let cancel = spawn_engine(engine);

    tokio::task::yield_now().await;

    // EV=0.02, confidence=0.5 → score=0.01 < threshold 0.04
    let signal = make_signal("mkt-slow", 0.02, 0.5);
    bus.publish(Event::Signal(signal)).unwrap();

    // We should NOT receive a FastSignal. A TopSignalsBatch is fine (slow path).
    let result = timeout(Duration::from_millis(500), async {
        loop {
            if let Ok(ev) = rx.recv().await {
                if let Event::FastSignal(_) = ev.as_ref() {
                    return true;
                }
            }
        }
    })
    .await;

    assert!(result.is_err(), "slow signal must not produce a FastSignal");

    cancel.cancel();
}

/// Negative EV must NOT route to fast path even if magnitude * confidence >= threshold.
#[tokio::test]
async fn negative_ev_not_fast() {
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let engine = SignalPriorityEngine::new(default_config(), bus.clone()).unwrap();
    let cancel = spawn_engine(engine);

    tokio::task::yield_now().await;

    // EV=-0.10, confidence=0.8 → score=-0.08, and EV<0 → must NOT be fast.
    let signal = make_signal("mkt-neg", -0.10, 0.8);
    bus.publish(Event::Signal(signal)).unwrap();

    let result = timeout(Duration::from_millis(200), async {
        loop {
            if let Ok(ev) = rx.recv().await {
                if let Event::FastSignal(_) = ev.as_ref() {
                    return true;
                }
            }
        }
    })
    .await;

    assert!(result.is_err(), "negative EV signal must not produce FastSignal");

    cancel.cancel();
}

/// Buffer several slow signals, wait for batch_window_ms flush, verify
/// TopSignalsBatch with top_n sorted descending by priority score.
#[tokio::test]
async fn batch_flush_emits_top_signals() {
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let config = PriorityConfig {
        batch_window_ms: 100, // short window
        top_n:           2,
        ..default_config()
    };
    let engine = SignalPriorityEngine::new(config, bus.clone()).unwrap();
    let cancel = spawn_engine(engine);

    tokio::task::yield_now().await;

    // Publish 4 slow signals with different scores, all above min_slow_ev.
    // score = ev * confidence
    bus.publish(Event::Signal(make_signal("m1", 0.02, 0.5))).unwrap(); // 0.010
    bus.publish(Event::Signal(make_signal("m2", 0.03, 0.5))).unwrap(); // 0.015
    bus.publish(Event::Signal(make_signal("m3", 0.03, 0.9))).unwrap(); // 0.027
    bus.publish(Event::Signal(make_signal("m4", 0.02, 0.8))).unwrap(); // 0.016

    // Wait for TopSignalsBatch.
    let batch = timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(ev) = rx.recv().await {
                if let Event::TopSignalsBatch(b) = ev.as_ref() {
                    return b.clone();
                }
            }
        }
    })
    .await
    .expect("timed out waiting for TopSignalsBatch");

    // top_n = 2, so we should get the two highest-scored signals.
    assert_eq!(batch.signals.len(), 2);

    // Verify descending order.
    assert!(batch.signals[0].priority_score >= batch.signals[1].priority_score);

    // Highest should be m3 (0.027), second m4 (0.016).
    assert_eq!(batch.signals[0].signal.market_id, "m3");
    assert_eq!(batch.signals[1].signal.market_id, "m4");

    cancel.cancel();
}

/// Signal with EV below min_slow_ev is dropped entirely (never appears in batch).
#[tokio::test]
async fn min_slow_ev_filter() {
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let config = PriorityConfig {
        batch_window_ms: 100,
        min_slow_ev:     0.05,
        ..default_config()
    };
    let engine = SignalPriorityEngine::new(config, bus.clone()).unwrap();
    let cancel = spawn_engine(engine);

    tokio::task::yield_now().await;

    // EV=0.02 < min_slow_ev=0.05 → dropped.
    bus.publish(Event::Signal(make_signal("dropped", 0.02, 0.5))).unwrap();

    // Also send one that passes the filter so we can verify the batch.
    bus.publish(Event::Signal(make_signal("kept", 0.06, 0.5))).unwrap();

    let batch = timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(ev) = rx.recv().await {
                if let Event::TopSignalsBatch(b) = ev.as_ref() {
                    return b.clone();
                }
            }
        }
    })
    .await
    .expect("timed out waiting for TopSignalsBatch");

    assert_eq!(batch.signals.len(), 1);
    assert_eq!(batch.signals[0].signal.market_id, "kept");

    cancel.cancel();
}

/// Invalid config (batch_window_ms=0) returns Err from SignalPriorityEngine::new.
#[tokio::test]
async fn config_validation() {
    let bus = EventBus::new();

    let bad_configs = vec![
        PriorityConfig { batch_window_ms: 0, ..default_config() },
        PriorityConfig { top_n: 0, ..default_config() },
        PriorityConfig { fast_raev_threshold: 0.0, ..default_config() },
        PriorityConfig { slow_queue_capacity: 0, ..default_config() },
    ];

    for cfg in bad_configs {
        let result = SignalPriorityEngine::new(cfg, bus.clone());
        assert!(result.is_err(), "expected Err for invalid config");
    }
}

/// Push more than slow_queue_capacity signals — lowest-scored are evicted.
/// Uses a long batch window and cancellation to trigger the final flush,
/// ensuring all signals are buffered before any flush occurs.
#[tokio::test]
async fn queue_capacity_eviction() {
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let config = PriorityConfig {
        batch_window_ms:     60_000, // very long — no timer flush during test
        top_n:               10,     // large enough to emit everything in queue
        slow_queue_capacity: 3,      // only room for 3
        ..default_config()
    };
    let engine = SignalPriorityEngine::new(config, bus.clone()).unwrap();
    let cancel = spawn_engine(engine);

    // Let the engine start and consume the initial (immediate) tick.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Publish 5 slow signals. All must stay below fast_raev_threshold (0.04).
    // Use low confidence so score = ev * confidence < 0.04.
    // Scores: 0.01*0.5=0.005, 0.02*0.5=0.010, 0.03*0.5=0.015, 0.04*0.5=0.020, 0.05*0.5=0.025
    for i in 1..=5u32 {
        let ev = 0.01 * i as f64;
        let sig = make_signal(&format!("cap-{i}"), ev, 0.5);
        bus.publish(Event::Signal(sig)).unwrap();
    }

    // Ensure all signals are processed by the engine before cancelling.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Cancel triggers final flush of whatever remains in the queue.
    cancel.cancel();

    // Collect all TopSignalsBatch events.  There may be more than one batch
    // (e.g. an empty initial flush), so we collect all non-empty batches.
    let mut all_signals = Vec::new();
    let _ = timeout(Duration::from_secs(2), async {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if let Event::TopSignalsBatch(b) = ev.as_ref() {
                        for s in &b.signals {
                            all_signals.push(s.clone());
                        }
                    }
                }
                Err(_) => break,
            }
        }
    })
    .await;

    // Only 3 should survive (the top 3 by score: cap-5, cap-4, cap-3).
    // Sort collected signals by priority_score descending for deterministic assertion.
    all_signals.sort_by(|a, b| b.priority_score.partial_cmp(&a.priority_score).unwrap());

    assert_eq!(all_signals.len(), 3, "expected 3 signals after capacity eviction, got {}: {:?}",
        all_signals.len(),
        all_signals.iter().map(|s| (&s.signal.market_id, s.priority_score)).collect::<Vec<_>>());
    assert_eq!(all_signals[0].signal.market_id, "cap-5");
    assert_eq!(all_signals[1].signal.market_id, "cap-4");
    assert_eq!(all_signals[2].signal.market_id, "cap-3");
}
