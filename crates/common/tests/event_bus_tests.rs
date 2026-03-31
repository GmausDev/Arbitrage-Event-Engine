// crates/common/tests/event_bus_tests.rs
//
// Integration tests for the EventBus broadcast channel wrapper.

use std::time::Duration;

use chrono::Utc;
use common::events::{Event, MarketUpdate};
use common::event_bus::EventBus;
use common::types::MarketNode;
use tokio::time::timeout;

/// Helper: build a minimal `Event::Market` with a given market ID.
fn market_event(id: &str) -> Event {
    Event::Market(MarketUpdate {
        market: MarketNode {
            id: id.to_string(),
            probability: 0.5,
            liquidity: 1000.0,
            last_update: Utc::now(),
        },
    })
}

#[tokio::test]
async fn publish_and_subscribe() {
    let bus = EventBus::new();
    let mut rx = bus.subscribe();

    let sent = market_event("MKT-1");
    let count = bus.publish(sent).unwrap();
    assert_eq!(count, 1, "should have exactly one subscriber");

    let received = timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("recv timed out")
        .expect("recv error");

    match received.as_ref() {
        Event::Market(update) => assert_eq!(update.market.id, "MKT-1"),
        other => panic!("expected Event::Market, got {:?}", other),
    }
}

#[tokio::test]
async fn multiple_subscribers_receive_same_event() {
    let bus = EventBus::new();
    let mut rx1 = bus.subscribe();
    let mut rx2 = bus.subscribe();
    let mut rx3 = bus.subscribe();

    let count = bus.publish(market_event("MKT-2")).unwrap();
    assert_eq!(count, 3, "three subscribers should be registered");

    for (i, rx) in [&mut rx1, &mut rx2, &mut rx3].iter_mut().enumerate() {
        let ev = timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap_or_else(|_| panic!("subscriber {i} timed out"))
            .unwrap_or_else(|e| panic!("subscriber {i} recv error: {e}"));

        match ev.as_ref() {
            Event::Market(update) => assert_eq!(update.market.id, "MKT-2"),
            other => panic!("subscriber {i}: expected Event::Market, got {:?}", other),
        }
    }
}

#[tokio::test]
async fn publish_returns_subscriber_count() {
    let bus = EventBus::new();

    // No subscribers: publish returns an error.
    let result = bus.publish(market_event("MKT-X"));
    assert!(result.is_err(), "publish with no subscribers should return Err");

    // Add subscribers one at a time and verify the count increases.
    let _rx1 = bus.subscribe();
    assert_eq!(bus.publish(market_event("MKT-X")).unwrap(), 1);

    let _rx2 = bus.subscribe();
    assert_eq!(bus.publish(market_event("MKT-X")).unwrap(), 2);

    let _rx3 = bus.subscribe();
    assert_eq!(bus.publish(market_event("MKT-X")).unwrap(), 3);
}

#[tokio::test]
async fn event_ordering_preserved() {
    let bus = EventBus::new();
    let mut rx = bus.subscribe();

    let n = 50;
    for i in 0..n {
        bus.publish(market_event(&format!("MKT-{i}"))).unwrap();
    }

    for i in 0..n {
        let ev = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("recv timed out")
            .expect("recv error");

        match ev.as_ref() {
            Event::Market(update) => {
                assert_eq!(update.market.id, format!("MKT-{i}"), "event {i} out of order");
            }
            other => panic!("expected Event::Market at index {i}, got {:?}", other),
        }
    }
}

#[tokio::test]
async fn subscribe_after_publish_misses_event() {
    let bus = EventBus::new();

    // Need at least one subscriber for publish to succeed.
    let _early_rx = bus.subscribe();

    bus.publish(market_event("MKT-OLD")).unwrap();

    // A late subscriber joins after the event was published.
    let mut late_rx = bus.subscribe();

    bus.publish(market_event("MKT-NEW")).unwrap();

    // The late subscriber should only receive MKT-NEW, not MKT-OLD.
    let ev = timeout(Duration::from_secs(1), late_rx.recv())
        .await
        .expect("recv timed out")
        .expect("recv error");

    match ev.as_ref() {
        Event::Market(update) => assert_eq!(update.market.id, "MKT-NEW"),
        other => panic!("expected Event::Market(MKT-NEW), got {:?}", other),
    }
}

#[tokio::test]
async fn capacity_overflow_causes_lag() {
    // Create a bus with a tiny capacity.
    let capacity = 4;
    let bus = EventBus::with_capacity(capacity);
    let mut rx = bus.subscribe();

    // Publish more events than the capacity without consuming any.
    let overflow_count = capacity + 10;
    for i in 0..overflow_count {
        // All publishes succeed because the sender doesn't block.
        bus.publish(market_event(&format!("MKT-{i}"))).unwrap();
    }

    // The first recv should report lagged (some events were dropped).
    let result = rx.recv().await;
    match result {
        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
            assert!(n > 0, "should have lagged by at least 1 event");
        }
        Ok(ev) => {
            // If we got an event, verify it's not the first one (i.e., some were skipped).
            // The broadcast channel may deliver a later event if the lag was already
            // accounted for internally. This is acceptable behavior.
            match ev.as_ref() {
                Event::Market(update) => {
                    assert_ne!(
                        update.market.id, "MKT-0",
                        "should have lost early events due to overflow"
                    );
                }
                _ => panic!("unexpected event variant"),
            }
        }
        Err(e) => panic!("unexpected error: {:?}", e),
    }
}
