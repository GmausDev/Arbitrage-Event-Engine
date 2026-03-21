// crates/portfolio_optimizer/tests/portfolio_optimizer.rs
//
// Integration tests for PortfolioOptimizer.
//
// Each test spins up a real EventBus, subscribes BEFORE spawning the engine,
// publishes one or more Event::Signal events, waits one tick window, and then
// asserts on the Event::OptimizedSignal(s) received.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use common::{Event, EventBus, OptimizedSignal, TradeDirection, TradeSignal};
use portfolio_optimizer::{AllocationConfig, PortfolioOptimizer};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A fast tick (20 ms) config for tests that need quick flushes.
fn fast_config() -> AllocationConfig {
    AllocationConfig {
        tick_ms: 20,
        // Tests publish via Event::Signal; disable the priority-engine path so
        // raw signals are processed directly rather than via TopSignalsBatch.
        use_priority_engine: false,
        ..Default::default()
    }
}

fn make_signal(market_id: &str, ev: f64, confidence: f64, pos_frac: f64) -> TradeSignal {
    TradeSignal {
        market_id:         market_id.to_string(),
        direction:         TradeDirection::Buy,
        expected_value:    ev,
        position_fraction: pos_frac,
        posterior_prob:    0.60,
        market_prob:       0.50,
        confidence,
        timestamp:         Utc::now(),
        source:            String::new(),
    }
}

/// Spawn the optimizer and return a cancel token.
fn spawn_optimizer(bus: &EventBus, config: AllocationConfig) -> (PortfolioOptimizer, CancellationToken) {
    let optimizer = PortfolioOptimizer::new(config, bus.clone());
    let cancel    = CancellationToken::new();
    (optimizer, cancel)
}

/// Collect up to `max_count` OptimizedSignals within `timeout_ms`.
async fn collect_optimized(
    mut rx:        tokio::sync::broadcast::Receiver<Arc<Event>>,
    max_count:     usize,
    timeout_ms:    u64,
) -> Vec<OptimizedSignal> {
    let mut results = Vec::new();
    let _ = timeout(Duration::from_millis(timeout_ms), async {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if let Event::OptimizedSignal(opt) = ev.as_ref() {
                        results.push(opt.clone());
                        if results.len() >= max_count {
                            return;
                        }
                    }
                }
                _ => return,
            }
        }
    })
    .await;
    results
}

// ---------------------------------------------------------------------------
// 1. Single signal is optimized and emitted
// ---------------------------------------------------------------------------

#[tokio::test]
async fn single_signal_emitted() {
    let bus = EventBus::new();
    let rx  = bus.subscribe();

    let (optimizer, cancel) = spawn_optimizer(&bus, fast_config());
    let c = cancel.clone();
    tokio::spawn(async move { optimizer.run(c).await });

    bus.publish(Event::Signal(make_signal("BTCUSD", 0.05, 0.80, 0.08))).unwrap();

    // Wait two tick windows to ensure flush happens
    let results = collect_optimized(rx, 1, 200).await;
    cancel.cancel();

    assert_eq!(results.len(), 1, "expected exactly one OptimizedSignal");
    assert_eq!(results[0].signal.market_id, "BTCUSD");
    // Requested 0.08 < max_per_market 0.10 and no niche pressure → should be 0.08
    assert!(
        (results[0].signal.position_fraction - 0.08).abs() < 1e-9,
        "allocation should be 0.08, got {}",
        results[0].signal.position_fraction
    );
}

// ---------------------------------------------------------------------------
// 2. Per-market cap enforced
// ---------------------------------------------------------------------------

#[tokio::test]
async fn per_market_cap_enforced() {
    let bus = EventBus::new();
    let rx  = bus.subscribe();

    let (optimizer, cancel) = spawn_optimizer(&bus, fast_config()); // max_per_market = 0.10
    let c = cancel.clone();
    tokio::spawn(async move { optimizer.run(c).await });

    // Request 25 % — must be capped to 10 %
    bus.publish(Event::Signal(make_signal("ETHUSD", 0.07, 0.80, 0.25))).unwrap();

    let results = collect_optimized(rx, 1, 200).await;
    cancel.cancel();

    let result = results.into_iter().next().expect("expected one OptimizedSignal");
    assert!(
        result.signal.position_fraction <= 0.10 + 1e-9,
        "allocation {} exceeds max_per_market 0.10",
        result.signal.position_fraction
    );
}

// ---------------------------------------------------------------------------
// 3. Duplicate market_id — highest RAEV wins
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dedup_highest_raev_wins() {
    let bus = EventBus::new();
    let rx  = bus.subscribe();

    let (optimizer, cancel) = spawn_optimizer(&bus, fast_config());
    let c = cancel.clone();
    tokio::spawn(async move { optimizer.run(c).await });

    // Two signals for same market — second has higher RAEV
    bus.publish(Event::Signal(make_signal("BTCUSD", 0.02, 0.50, 0.05))).unwrap(); // RAEV=0.01
    bus.publish(Event::Signal(make_signal("BTCUSD", 0.10, 0.90, 0.09))).unwrap(); // RAEV=0.09

    let results = collect_optimized(rx, 1, 200).await;
    cancel.cancel();

    assert_eq!(results.len(), 1, "dedup must collapse both signals to one");
    assert!(
        (results[0].risk_adjusted_ev - 0.09).abs() < 1e-9,
        "expected RAEV 0.09, got {}",
        results[0].risk_adjusted_ev
    );
}

// ---------------------------------------------------------------------------
// 4. Two correlated markets — second gets correlation penalty
// ---------------------------------------------------------------------------

#[tokio::test]
async fn correlated_markets_get_penalty() {
    let config = AllocationConfig {
        tick_ms:                    20,
        max_allocation_per_niche:   0.20,
        correlation_penalty_factor: 1.0,
        default_cluster_correlation: 0.5,
        max_allocation_per_market:  0.10,
        total_capital:              1.0,
        bankroll:                   10_000.0,
        use_priority_engine:        false,
    };
    let bus = EventBus::new();
    let rx  = bus.subscribe();

    let (optimizer, cancel) = spawn_optimizer(&bus, config);
    let c = cancel.clone();
    tokio::spawn(async move { optimizer.run(c).await });

    // Both in cluster "US" — first gets full allocation, second should be penalised
    bus.publish(Event::Signal(make_signal("US_ELECTION_TRUMP",  0.10, 0.90, 0.10))).unwrap();
    bus.publish(Event::Signal(make_signal("US_ELECTION_HARRIS", 0.08, 0.80, 0.10))).unwrap();

    let results = collect_optimized(rx, 2, 300).await;
    cancel.cancel();

    assert_eq!(results.len(), 2);
    let trump  = results.iter().find(|s| s.signal.market_id == "US_ELECTION_TRUMP").unwrap();
    let harris = results.iter().find(|s| s.signal.market_id == "US_ELECTION_HARRIS").unwrap();

    // Trump: first allocated, niche_util=0 → no penalty
    assert!((trump.correlation_penalty).abs() < 1e-9,
        "first market should have zero penalty, got {}", trump.correlation_penalty);

    // Harris: second in niche, should have a positive penalty → reduced allocation
    assert!(harris.signal.position_fraction < 0.10,
        "second correlated market should have reduced allocation, got {}",
        harris.signal.position_fraction);
}

// ---------------------------------------------------------------------------
// 5. Total capital cap respected across multiple signals
// ---------------------------------------------------------------------------

#[tokio::test]
async fn total_capital_cap_respected() {
    let config = AllocationConfig {
        tick_ms:                   20,
        total_capital:             0.15,
        max_allocation_per_market: 0.10,
        ..Default::default()
    };
    let bus = EventBus::new();
    let rx  = bus.subscribe();

    let (optimizer, cancel) = spawn_optimizer(&bus, config);
    let c = cancel.clone();
    tokio::spawn(async move { optimizer.run(c).await });

    // Three signals each requesting 0.10; total budget = 0.15
    bus.publish(Event::Signal(make_signal("MKT_A", 0.09, 0.90, 0.10))).unwrap();
    bus.publish(Event::Signal(make_signal("MKT_B", 0.07, 0.80, 0.10))).unwrap();
    bus.publish(Event::Signal(make_signal("MKT_C", 0.05, 0.70, 0.10))).unwrap();

    let results = collect_optimized(rx, 3, 300).await;
    cancel.cancel();

    let total: f64 = results.iter().map(|s| s.signal.position_fraction).sum();
    assert!(
        total <= 0.15 + 1e-9,
        "sum of allocations {total:.4} exceeds total_capital 0.15"
    );
}

// ---------------------------------------------------------------------------
// 6. No signals → no OptimizedSignal events published
// ---------------------------------------------------------------------------

#[tokio::test]
async fn empty_batch_emits_nothing() {
    let bus = EventBus::new();
    let rx  = bus.subscribe();

    let (optimizer, cancel) = spawn_optimizer(&bus, fast_config());
    let c = cancel.clone();
    tokio::spawn(async move { optimizer.run(c).await });

    // Wait two tick windows without publishing any signals
    let results = collect_optimized(rx, 1, 150).await;
    cancel.cancel();

    assert!(results.is_empty(), "empty batch must not emit any OptimizedSignal");
}

// ---------------------------------------------------------------------------
// 7. RAEV ordering drives allocation priority under a budget constraint
// ---------------------------------------------------------------------------

#[tokio::test]
async fn high_raev_signal_gets_priority() {
    let config = AllocationConfig {
        tick_ms:                   20,
        total_capital:             0.12,
        max_allocation_per_market: 0.10,
        use_priority_engine:       false,
        ..Default::default()
    };
    let bus = EventBus::new();
    let rx  = bus.subscribe();

    let (optimizer, cancel) = spawn_optimizer(&bus, config);
    let c = cancel.clone();
    tokio::spawn(async move { optimizer.run(c).await });

    // HIGH_EV: RAEV = 0.09 → should receive 0.10 (full)
    // LOW_EV:  RAEV = 0.01 → only 0.02 budget remaining
    bus.publish(Event::Signal(make_signal("LOW_EV",  0.02, 0.50, 0.10))).unwrap();
    bus.publish(Event::Signal(make_signal("HIGH_EV", 0.10, 0.90, 0.10))).unwrap();

    let results = collect_optimized(rx, 2, 300).await;
    cancel.cancel();

    let high = results.iter().find(|s| s.signal.market_id == "HIGH_EV").expect("HIGH_EV missing");
    let low  = results.iter().find(|s| s.signal.market_id == "LOW_EV").expect("LOW_EV missing");

    assert!(
        (high.signal.position_fraction - 0.10).abs() < 1e-9,
        "HIGH_EV should get 0.10, got {}",
        high.signal.position_fraction
    );
    assert!(
        (low.signal.position_fraction - 0.02).abs() < 1e-9,
        "LOW_EV should get 0.02 (remaining budget), got {}",
        low.signal.position_fraction
    );
}

// ---------------------------------------------------------------------------
// 8. risk_adjusted_ev field is populated correctly
// ---------------------------------------------------------------------------

#[tokio::test]
async fn risk_adjusted_ev_field_correct() {
    let bus = EventBus::new();
    let rx  = bus.subscribe();

    let (optimizer, cancel) = spawn_optimizer(&bus, fast_config());
    let c = cancel.clone();
    tokio::spawn(async move { optimizer.run(c).await });

    // RAEV = ev × confidence = 0.06 × 0.75 = 0.045
    bus.publish(Event::Signal(make_signal("MARKET_X", 0.06, 0.75, 0.05))).unwrap();

    let results = collect_optimized(rx, 1, 200).await;
    cancel.cancel();

    let result = results.into_iter().next().expect("expected one OptimizedSignal");
    assert!(
        (result.risk_adjusted_ev - 0.045).abs() < 1e-9,
        "expected risk_adjusted_ev = 0.045, got {}",
        result.risk_adjusted_ev
    );
}
