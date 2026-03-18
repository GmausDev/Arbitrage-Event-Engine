// crates/execution_sim/tests/execution_sim.rs
//
// Integration tests for ExecutionSimulator.
//
// Each test spins up a real EventBus, subscribes BEFORE spawning the engine,
// publishes one or more Event::ApprovedTrade events, waits for the resulting
// Event::Execution / Event::Portfolio events, and asserts on the output.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use common::{ApprovedTrade, Event, EventBus, ExecutionResult, TradeDirection};
use execution_sim::{ExecutionConfig, ExecutionSimulator};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_trade(market_id: &str, fraction: f64, market_prob: f64) -> ApprovedTrade {
    let now = Utc::now();
    ApprovedTrade {
        market_id:         market_id.to_string(),
        direction:         TradeDirection::Buy,
        approved_fraction: fraction,
        expected_value:    0.05,
        posterior_prob:    market_prob + 0.10,
        market_prob,
        signal_timestamp:  now,
        timestamp:         now,
    }
}

/// Collect up to `max_count` ExecutionResult events within `timeout_ms`.
async fn collect_executions(
    mut rx:       tokio::sync::broadcast::Receiver<Arc<Event>>,
    max_count:    usize,
    timeout_ms:   u64,
) -> Vec<ExecutionResult> {
    let mut results = Vec::new();
    let _ = timeout(Duration::from_millis(timeout_ms), async {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if let Event::Execution(r) = ev.as_ref() {
                        results.push(r.clone());
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

/// Count PortfolioUpdate events received within `timeout_ms`.
async fn count_portfolio_updates(
    mut rx:     tokio::sync::broadcast::Receiver<Arc<Event>>,
    max_count:  usize,
    timeout_ms: u64,
) -> usize {
    let mut count = 0;
    let _ = timeout(Duration::from_millis(timeout_ms), async {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if let Event::Portfolio(_) = ev.as_ref() {
                        count += 1;
                        if count >= max_count {
                            return;
                        }
                    }
                }
                _ => return,
            }
        }
    })
    .await;
    count
}

fn spawn_sim(bus: &EventBus, config: ExecutionConfig) -> CancellationToken {
    let sim    = ExecutionSimulator::new(config, bus.clone());
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move { sim.run(c).await });
    cancel
}

// ---------------------------------------------------------------------------
// 1. Single trade produces an ExecutionResult
// ---------------------------------------------------------------------------

#[tokio::test]
async fn single_trade_emits_execution_result() {
    let bus = EventBus::new();
    let rx  = bus.subscribe();

    let cancel = spawn_sim(&bus, ExecutionConfig::default());

    bus.publish(Event::ApprovedTrade(make_trade("BTCUSD", 0.05, 0.50))).unwrap();

    let results = collect_executions(rx, 1, 300).await;
    cancel.cancel();

    assert_eq!(results.len(), 1, "expected exactly one ExecutionResult");
    assert_eq!(results[0].trade.market_id, "BTCUSD");
    assert!(results[0].executed_quantity >= 0.0);
    assert!(results[0].executed_quantity <= 0.05 + 1e-9);
}

// ---------------------------------------------------------------------------
// 2. No slippage mode — avg_price equals market_prob exactly
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_slippage_avg_price_equals_market_prob() {
    let config = ExecutionConfig {
        simulate_slippage:        false,
        partial_fill_probability: 0.0, // also disable partial fills for clarity
        ..Default::default()
    };
    let bus = EventBus::new();
    let rx  = bus.subscribe();

    let cancel = spawn_sim(&bus, config);

    bus.publish(Event::ApprovedTrade(make_trade("ETHUSD", 0.05, 0.60))).unwrap();

    let results = collect_executions(rx, 1, 300).await;
    cancel.cancel();

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].avg_price - 0.60).abs() < 1e-9,
        "with no slippage avg_price should equal market_prob (0.60), got {}",
        results[0].avg_price
    );
    assert_eq!(results[0].slippage, 0.0);
}

// ---------------------------------------------------------------------------
// 3. Full-fill mode — fill_ratio = 1.0
// ---------------------------------------------------------------------------

#[tokio::test]
async fn full_fill_mode_fill_ratio_is_one() {
    let config = ExecutionConfig {
        simulate_slippage:        false,
        partial_fill_probability: 0.0, // never partial
        ..Default::default()
    };
    let bus = EventBus::new();
    let rx  = bus.subscribe();

    let cancel = spawn_sim(&bus, config);

    bus.publish(Event::ApprovedTrade(make_trade("SOLUSD", 0.08, 0.40))).unwrap();

    let results = collect_executions(rx, 1, 300).await;
    cancel.cancel();

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].fill_ratio - 1.0).abs() < 1e-9,
        "expected fill_ratio=1.0, got {}",
        results[0].fill_ratio
    );
    assert!(
        (results[0].executed_quantity - 0.08).abs() < 1e-9,
        "expected executed_quantity=0.08, got {}",
        results[0].executed_quantity
    );
    assert!(results[0].filled);
}

// ---------------------------------------------------------------------------
// 4. Partial fill — executed_quantity < approved_fraction
// ---------------------------------------------------------------------------

#[tokio::test]
async fn always_partial_reduces_quantity() {
    let config = ExecutionConfig {
        simulate_slippage:        false,
        partial_fill_probability: 1.0, // always partial
        ..Default::default()
    };
    let bus = EventBus::new();
    let rx  = bus.subscribe();

    let cancel = spawn_sim(&bus, config);

    bus.publish(Event::ApprovedTrade(make_trade("XRPUSD", 0.10, 0.55))).unwrap();

    let results = collect_executions(rx, 1, 300).await;
    cancel.cancel();

    assert_eq!(results.len(), 1);
    assert!(
        results[0].executed_quantity < 0.10,
        "partial fill must reduce quantity below 0.10, got {}",
        results[0].executed_quantity
    );
    assert!(
        results[0].fill_ratio >= 0.5 - 1e-9 && results[0].fill_ratio < 1.0,
        "fill_ratio must be in [0.5, 1.0), got {}",
        results[0].fill_ratio
    );
}

// ---------------------------------------------------------------------------
// 5. Portfolio update published for every filled trade
// ---------------------------------------------------------------------------

#[tokio::test]
async fn portfolio_update_published_after_fill() {
    let config = ExecutionConfig {
        simulate_slippage:        false,
        partial_fill_probability: 0.0,
        ..Default::default()
    };
    let bus = EventBus::new();
    let rx  = bus.subscribe();

    let cancel = spawn_sim(&bus, config);

    bus.publish(Event::ApprovedTrade(make_trade("MKTA", 0.05, 0.50))).unwrap();
    bus.publish(Event::ApprovedTrade(make_trade("MKTB", 0.05, 0.45))).unwrap();

    let count = count_portfolio_updates(rx, 2, 400).await;
    cancel.cancel();

    assert_eq!(count, 2, "one PortfolioUpdate per trade, got {count}");
}

// ---------------------------------------------------------------------------
// 6. Portfolio exposure increases after each fill
// ---------------------------------------------------------------------------

#[tokio::test]
async fn portfolio_exposure_increases() {
    let config = ExecutionConfig {
        simulate_slippage:        false,
        partial_fill_probability: 0.0,
        ..Default::default()
    };
    let bus = EventBus::new();
    let rx  = bus.subscribe();

    let cancel = spawn_sim(&bus, config);

    bus.publish(Event::ApprovedTrade(make_trade("MKT1", 0.05, 0.50))).unwrap();
    bus.publish(Event::ApprovedTrade(make_trade("MKT2", 0.07, 0.55))).unwrap();

    // Collect two PortfolioUpdate events and check the final exposure.
    let mut last_exposure = 0.0_f64;
    let mut rx2 = bus.subscribe();
    let _ = timeout(Duration::from_millis(400), async {
        let mut count = 0;
        loop {
            match rx2.recv().await {
                Ok(ev) => {
                    if let Event::Portfolio(pu) = ev.as_ref() {
                        last_exposure = pu.portfolio.exposure;
                        count += 1;
                        if count >= 2 {
                            return;
                        }
                    }
                }
                _ => return,
            }
        }
    })
    .await;
    cancel.cancel();

    // Both positions filled → exposure ≈ 0.05 + 0.07 = 0.12 (in bankroll fractions)
    assert!(
        last_exposure > 0.0,
        "exposure must be positive after fills, got {last_exposure}"
    );
}

// ---------------------------------------------------------------------------
// 7. Unit test: simulate_execution with seeded RNG — no slippage
// ---------------------------------------------------------------------------

#[test]
fn unit_simulate_execution_no_slippage() {
    use execution_sim::ExecutionOrder;
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use chrono::Utc;

    let config = ExecutionConfig {
        simulate_slippage:        false,
        partial_fill_probability: 0.0,
        ..Default::default()
    };
    let order = ExecutionOrder {
        market_id:    "TEST".into(),
        direction:    TradeDirection::Buy,
        quantity:     0.10,
        price:        0.50,
        max_slippage: 0.05,
        timestamp:    Utc::now(),
    };
    let trade = ApprovedTrade {
        market_id:         "TEST".into(),
        direction:         TradeDirection::Buy,
        approved_fraction: 0.10,
        expected_value:    0.05,
        posterior_prob:    0.60,
        market_prob:       0.50,
        timestamp:         Utc::now(),
    };
    let mut rng = StdRng::seed_from_u64(0);
    let result = ExecutionSimulator::simulate_execution(&config, &order, &mut rng, trade);

    assert!(result.filled);
    assert!((result.slippage).abs() < 1e-12, "slippage must be zero");
    assert!((result.avg_price - 0.50).abs() < 1e-9);
    assert!((result.executed_quantity - 0.10).abs() < 1e-9);
    assert!((result.fill_ratio - 1.0).abs() < 1e-9);
}

// ---------------------------------------------------------------------------
// 8. Unit test: simulate_execution with slippage — price is perturbed
// ---------------------------------------------------------------------------

#[test]
fn unit_simulate_execution_slippage_perturbs_price() {
    use execution_sim::ExecutionOrder;
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use chrono::Utc;

    let config = ExecutionConfig {
        simulate_slippage:        true,
        slippage_std_dev:         0.01,  // 1 % std dev
        partial_fill_probability: 0.0,
        ..Default::default()
    };
    let order = ExecutionOrder {
        market_id:    "SLIPTEST".into(),
        direction:    TradeDirection::Buy,
        quantity:     0.05,
        price:        0.50,
        max_slippage: 0.10,  // 10 % cap
        timestamp:    Utc::now(),
    };
    let trade = ApprovedTrade {
        market_id:         "SLIPTEST".into(),
        direction:         TradeDirection::Buy,
        approved_fraction: 0.05,
        expected_value:    0.05,
        posterior_prob:    0.60,
        market_prob:       0.50,
        timestamp:         Utc::now(),
    };
    let mut rng = StdRng::seed_from_u64(42);
    let result = ExecutionSimulator::simulate_execution(&config, &order, &mut rng, trade);

    assert!(result.filled);
    // avg_price must stay in (0, 1]
    assert!(result.avg_price > 0.0 && result.avg_price <= 1.0,
        "avg_price out of range: {}", result.avg_price);
    // slippage magnitude must not exceed max_slippage cap
    assert!(
        result.slippage.abs() <= 0.10 + 1e-9,
        "slippage {} exceeds max_slippage 0.10",
        result.slippage
    );
}

// ---------------------------------------------------------------------------
// 9. No ApprovedTrade events → no ExecutionResult events
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_trades_no_execution_events() {
    let bus = EventBus::new();
    let rx  = bus.subscribe();

    let cancel = spawn_sim(&bus, ExecutionConfig::default());

    let results = collect_executions(rx, 1, 150).await;
    cancel.cancel();

    assert!(results.is_empty(), "no trades → no execution results");
}
