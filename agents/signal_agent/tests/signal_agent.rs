// agents/signal_agent/tests/signal_agent.rs
// Integration tests for SignalAgent: event handling, gating, EV, Kelly sizing.
//
// Each test spins up a real EventBus, spawns the agent, sends events, and
// collects resulting TradeSignal events from the bus with a short timeout.

use std::time::Duration;

use chrono::Utc;
use common::{Event, EventBus, MarketNode, PosteriorUpdate, TradeDirection, TradeSignal};
use std::sync::Arc;
use signal_agent::{SignalAgent, SignalAgentConfig};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Default config calibrated so the spec example generates a signal.
fn test_config() -> SignalAgentConfig {
    SignalAgentConfig {
        min_expected_value: 0.01,
        max_position_fraction: 0.10,
        kelly_fraction: 0.25,
        max_market_price: 0.95,
        min_market_price: 0.05,
        min_confidence: 0.30,
        slippage_bps: 10.0,
        fee_bps: 5.0, // trading_cost = 0.0015
    }
}

/// Build a PosteriorUpdate with the given fields; all others zeroed/defaulted.
fn posterior(market_id: &str, posterior_prob: f64, confidence: f64) -> PosteriorUpdate {
    PosteriorUpdate {
        market_id: market_id.to_string(),
        prior_prob: 0.5,
        posterior_prob,
        market_prob: None,
        deviation: 0.0,
        confidence,
        timestamp: Utc::now(),
    }
}

/// Build a MarketUpdate with the given market_id and probability.
fn market_update(market_id: &str, probability: f64) -> Event {
    Event::Market(common::MarketUpdate {
        market: MarketNode {
            id: market_id.to_string(),
            probability,
            liquidity: 100_000.0,
            last_update: Utc::now(),
        },
    })
}

/// Drain the receiver and return the first `TradeSignal` received, or `None`
/// if none arrives within `timeout_ms`.
///
/// IMPORTANT: `rx` must have been subscribed *before* the events that
/// could generate the signal were published — otherwise the signal may have
/// been sent and missed before this function runs.
async fn collect_signal_from(
    mut rx: tokio::sync::broadcast::Receiver<std::sync::Arc<Event>>,
    timeout_ms: u64,
) -> Option<TradeSignal> {
    let result = timeout(Duration::from_millis(timeout_ms), async {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if let Event::Signal(signal) = ev.as_ref() {
                        return Some(signal.clone());
                    }
                }
                Err(_) => return None,
            }
        }
    })
    .await;
    result.unwrap_or(None)
}

/// Spawn the agent and return the cancel token.
fn spawn_agent(bus: &EventBus, config: SignalAgentConfig) -> CancellationToken {
    let cancel = CancellationToken::new();
    let agent = SignalAgent::new(bus.clone(), config);
    tokio::spawn(agent.run(cancel.child_token()));
    cancel
}

// ---------------------------------------------------------------------------
// 1. EV calculation correctness
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ev_buy_spec_example_generates_signal() {
    // posterior=0.63, market=0.54, cost≈0.0015 → EV ≈ 0.0885 > min_ev=0.01
    let bus = EventBus::new();
    let mut rx = bus.subscribe(); // subscribe BEFORE spawning
    let cancel = spawn_agent(&bus, test_config());

    bus.publish(Event::Posterior(posterior("MARKET-A", 0.63, 0.80)))
        .unwrap();
    bus.publish(market_update("MARKET-A", 0.54)).unwrap();

    // Collect signal
    let signal = timeout(Duration::from_millis(500), async {
        loop {
            if let Ok(ev) = rx.recv().await {
                if let Event::Signal(s) = ev.as_ref() {
                    return Some(s.clone());
                }
            }
        }
    })
    .await
    .ok()
    .flatten();

    cancel.cancel();

    let signal = signal.expect("should emit a BUY signal");
    assert_eq!(signal.market_id, "MARKET-A");
    assert_eq!(signal.direction, TradeDirection::Buy);
    assert!(signal.expected_value > 0.08, "EV={}", signal.expected_value);
    assert!(signal.position_fraction > 0.0);
    assert!(signal.position_fraction <= 0.10);
}

// ---------------------------------------------------------------------------
// 2. Kelly sizing correctness
// ---------------------------------------------------------------------------

#[tokio::test]
async fn kelly_fraction_is_within_bounds() {
    // posterior=0.70, market=0.50 → raw kelly=(0.20/0.50)=0.40; ×0.25=0.10 → clamped to 0.10
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let cancel = spawn_agent(&bus, test_config());

    bus.publish(Event::Posterior(posterior("MARKET-K", 0.70, 0.90)))
        .unwrap();
    bus.publish(market_update("MARKET-K", 0.50)).unwrap();

    let signal = timeout(Duration::from_millis(500), async {
        loop {
            if let Ok(ev) = rx.recv().await {
                if let Event::Signal(s) = ev.as_ref() {
                    return Some(s.clone());
                }
            }
        }
    })
    .await
    .ok()
    .flatten()
    .expect("signal should be emitted");

    cancel.cancel();

    assert!(
        signal.position_fraction <= test_config().max_position_fraction + 1e-9,
        "position_fraction={} exceeds max",
        signal.position_fraction
    );
    assert!(signal.position_fraction > 0.0);
}

// ---------------------------------------------------------------------------
// 3. Signal generation when EV > threshold
// ---------------------------------------------------------------------------

#[tokio::test]
async fn signal_emitted_when_ev_above_threshold() {
    let bus = EventBus::new();
    let signal = {
        let mut rx = bus.subscribe();
        let cancel = spawn_agent(&bus, test_config());

        bus.publish(Event::Posterior(posterior("MARKET-B", 0.65, 0.80)))
            .unwrap();
        bus.publish(market_update("MARKET-B", 0.50)).unwrap();

        let s = timeout(Duration::from_millis(500), async {
            loop {
                if let Ok(ev) = rx.recv().await {
                    if let Event::Signal(sig) = ev.as_ref() {
                        return Some(sig.clone());
                    }
                }
            }
        })
        .await
        .ok()
        .flatten();
        cancel.cancel();
        s
    };

    assert!(signal.is_some(), "expected a TradeSignal");
}

// ---------------------------------------------------------------------------
// 4. No signal when EV < threshold
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_signal_when_ev_below_threshold() {
    let mut cfg = test_config();
    cfg.min_expected_value = 0.20; // very high bar — gap of 0.05 won't clear it

    let bus = EventBus::new();
    // Subscribe BEFORE spawning and publishing so we don't miss any signal.
    let rx = bus.subscribe();
    let cancel = spawn_agent(&bus, cfg);

    bus.publish(Event::Posterior(posterior("MARKET-C", 0.55, 0.80)))
        .unwrap();
    bus.publish(market_update("MARKET-C", 0.50)).unwrap();

    let signal = collect_signal_from(rx, 300).await;
    cancel.cancel();
    assert!(signal.is_none(), "should not emit signal when EV < min_ev");
}

// ---------------------------------------------------------------------------
// 5. BUY vs SELL detection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn buy_direction_when_posterior_above_market() {
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let cancel = spawn_agent(&bus, test_config());

    bus.publish(Event::Posterior(posterior("MARKET-D", 0.70, 0.80)))
        .unwrap();
    bus.publish(market_update("MARKET-D", 0.50)).unwrap();

    let signal = timeout(Duration::from_millis(500), async {
        loop {
            if let Ok(ev) = rx.recv().await {
                if let Event::Signal(s) = ev.as_ref() {
                    return Some(s.clone());
                }
            }
        }
    })
    .await
    .ok()
    .flatten()
    .expect("signal");

    cancel.cancel();
    assert_eq!(signal.direction, TradeDirection::Buy);
}

#[tokio::test]
async fn sell_direction_when_posterior_below_market() {
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let cancel = spawn_agent(&bus, test_config());

    // posterior=0.35, market=0.60 → EV_sell = 0.60−0.35−cost > min_ev
    bus.publish(Event::Posterior(posterior("MARKET-E", 0.35, 0.80)))
        .unwrap();
    bus.publish(market_update("MARKET-E", 0.60)).unwrap();

    let signal = timeout(Duration::from_millis(500), async {
        loop {
            if let Ok(ev) = rx.recv().await {
                if let Event::Signal(s) = ev.as_ref() {
                    return Some(s.clone());
                }
            }
        }
    })
    .await
    .ok()
    .flatten()
    .expect("signal");

    cancel.cancel();
    assert_eq!(signal.direction, TradeDirection::Sell);
}

// ---------------------------------------------------------------------------
// 6. MarketUpdate arrives before PosteriorUpdate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn signal_emitted_when_market_arrives_first() {
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let cancel = spawn_agent(&bus, test_config());

    // Market price arrives first — no posterior yet, no signal.
    bus.publish(market_update("MARKET-F", 0.50)).unwrap();
    // Small delay to let agent process the MarketUpdate.
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Now posterior arrives — agent has the price stored, should generate signal.
    bus.publish(Event::Posterior(posterior("MARKET-F", 0.70, 0.80)))
        .unwrap();

    let signal = timeout(Duration::from_millis(500), async {
        loop {
            if let Ok(ev) = rx.recv().await {
                if let Event::Signal(s) = ev.as_ref() {
                    return Some(s.clone());
                }
            }
        }
    })
    .await
    .ok()
    .flatten();

    cancel.cancel();
    assert!(
        signal.is_some(),
        "signal should be emitted when posterior follows market price"
    );
}

// ---------------------------------------------------------------------------
// 7. PosteriorUpdate arrives before MarketUpdate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn signal_emitted_when_posterior_arrives_first() {
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let cancel = spawn_agent(&bus, test_config());

    // Posterior arrives first — no market price yet, no signal.
    bus.publish(Event::Posterior(posterior("MARKET-G", 0.70, 0.80)))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Market price arrives — agent has the belief stored, should generate signal.
    bus.publish(market_update("MARKET-G", 0.50)).unwrap();

    let signal = timeout(Duration::from_millis(500), async {
        loop {
            if let Ok(ev) = rx.recv().await {
                if let Event::Signal(s) = ev.as_ref() {
                    return Some(s.clone());
                }
            }
        }
    })
    .await
    .ok()
    .flatten();

    cancel.cancel();
    assert!(
        signal.is_some(),
        "signal should be emitted when market price follows posterior"
    );
}

// ---------------------------------------------------------------------------
// 8. Confidence filter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_signal_when_confidence_below_minimum() {
    let mut cfg = test_config();
    cfg.min_confidence = 0.80;

    let bus = EventBus::new();
    // Subscribe BEFORE spawning and publishing.
    let rx = bus.subscribe();
    let cancel = spawn_agent(&bus, cfg);

    // Confidence = 0.50 < 0.80 → no signal regardless of EV
    bus.publish(Event::Posterior(posterior("MARKET-H", 0.70, 0.50)))
        .unwrap();
    bus.publish(market_update("MARKET-H", 0.50)).unwrap();

    let signal = collect_signal_from(rx, 300).await;
    cancel.cancel();
    assert!(signal.is_none(), "should not emit signal with low confidence");
}

#[tokio::test]
async fn signal_emitted_when_confidence_meets_minimum() {
    let mut cfg = test_config();
    cfg.min_confidence = 0.80;

    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let cancel = spawn_agent(&bus, cfg);

    // Confidence = 0.85 ≥ 0.80 → signal should be emitted
    bus.publish(Event::Posterior(posterior("MARKET-I", 0.70, 0.85)))
        .unwrap();
    bus.publish(market_update("MARKET-I", 0.50)).unwrap();

    let signal = timeout(Duration::from_millis(500), async {
        loop {
            if let Ok(ev) = rx.recv().await {
                if let Event::Signal(s) = ev.as_ref() {
                    return Some(s.clone());
                }
            }
        }
    })
    .await
    .ok()
    .flatten();

    cancel.cancel();
    assert!(signal.is_some(), "signal should be emitted with sufficient confidence");
}

// ---------------------------------------------------------------------------
// 9. Price bounds filter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_signal_when_market_price_too_high() {
    let bus = EventBus::new();
    // Subscribe BEFORE spawning and publishing.
    let rx = bus.subscribe();
    let cancel = spawn_agent(&bus, test_config());

    // market_price = 0.96 > max_market_price = 0.95
    bus.publish(Event::Posterior(posterior("MARKET-J", 0.99, 0.90)))
        .unwrap();
    bus.publish(market_update("MARKET-J", 0.96)).unwrap();

    let signal = collect_signal_from(rx, 300).await;
    cancel.cancel();
    assert!(signal.is_none(), "should not emit signal for near-certain market");
}

#[tokio::test]
async fn no_signal_when_market_price_too_low() {
    let bus = EventBus::new();
    // Subscribe BEFORE spawning and publishing.
    let rx = bus.subscribe();
    let cancel = spawn_agent(&bus, test_config());

    // market_price = 0.03 < min_market_price = 0.05
    bus.publish(Event::Posterior(posterior("MARKET-L", 0.10, 0.90)))
        .unwrap();
    bus.publish(market_update("MARKET-L", 0.03)).unwrap();

    let signal = collect_signal_from(rx, 300).await;
    cancel.cancel();
    assert!(signal.is_none(), "should not emit signal for near-impossible market");
}

// ---------------------------------------------------------------------------
// 10. Signal fields populated correctly
// ---------------------------------------------------------------------------

#[tokio::test]
async fn signal_fields_match_inputs() {
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let cancel = spawn_agent(&bus, test_config());

    let post = 0.63_f64;
    let mkt = 0.54_f64;
    let conf = 0.80_f64;

    bus.publish(Event::Posterior(posterior("MARKET-M", post, conf)))
        .unwrap();
    bus.publish(market_update("MARKET-M", mkt)).unwrap();

    let signal = timeout(Duration::from_millis(500), async {
        loop {
            if let Ok(ev) = rx.recv().await {
                if let Event::Signal(s) = ev.as_ref() {
                    return Some(s.clone());
                }
            }
        }
    })
    .await
    .ok()
    .flatten()
    .expect("signal");

    cancel.cancel();

    assert_eq!(signal.market_id, "MARKET-M");
    assert!((signal.posterior_prob - post).abs() < 1e-10);
    assert!((signal.market_prob - mkt).abs() < 1e-10);
    assert!((signal.confidence - conf).abs() < 1e-10);
    assert!(signal.expected_value > 0.0);
    assert!(signal.position_fraction > 0.0);
    assert!(signal.position_fraction <= test_config().max_position_fraction + 1e-9);
}
