// crates/portfolio_engine/tests/portfolio_engine.rs
//
// Unit and integration tests for PortfolioEngine.
//
// Unit tests call the pure `apply_execution`, `recalculate_unrealized`, and
// `apply_risk_adjustments` functions directly with constructed state — no async
// required.
//
// Integration tests spin up a real EventBus, subscribe before spawning the
// engine, publish `Event::Execution` events, and assert on the resulting
// `Event::Portfolio` updates.

use std::collections::HashMap;
use std::time::Duration;

use chrono::Utc;
use common::{ApprovedTrade, Event, EventBus, ExecutionResult, TradeDirection};
use portfolio_engine::{PortfolioConfig, PortfolioEngine, PortfolioState};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

const BANKROLL: f64 = 10_000.0;

fn make_result(
    market_id:          &str,
    direction:          TradeDirection,
    executed_quantity:  f64, // fraction of bankroll
    avg_price:          f64,
    market_prob:        f64,
) -> ExecutionResult {
    let now = Utc::now();
    let trade = ApprovedTrade {
        market_id:         market_id.to_string(),
        direction,
        approved_fraction: executed_quantity,
        expected_value:    0.05,
        posterior_prob:    market_prob + 0.10,
        market_prob,
        signal_source:     "test".to_string(),
        signal_timestamp:  now,
        timestamp:         now,
    };
    ExecutionResult {
        trade,
        filled:            true,
        fill_ratio:        1.0,
        executed_quantity,
        avg_price,
        slippage:          0.0,
        timestamp:         Utc::now(),
    }
}

fn make_unfilled_result(market_id: &str) -> ExecutionResult {
    let mut r = make_result(market_id, TradeDirection::Buy, 0.05, 0.50, 0.50);
    r.filled       = false;
    r.fill_ratio   = 0.0;
    r.executed_quantity = 0.0;
    r
}

fn fresh_state() -> PortfolioState {
    PortfolioState::new(BANKROLL)
}

fn spawn_engine(bus: &EventBus, config: PortfolioConfig) -> CancellationToken {
    let engine = PortfolioEngine::new(config, bus.clone());
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move { engine.run(c).await });
    cancel
}

// ---------------------------------------------------------------------------
// 1. Unit: apply_execution creates a new position
// ---------------------------------------------------------------------------

#[test]
fn apply_execution_creates_position() {
    let mut state  = fresh_state();
    let result     = make_result("MKT1", TradeDirection::Buy, 0.05, 0.50, 0.50);
    let size_usd   = 0.05 * BANKROLL; // 500.0

    PortfolioEngine::apply_execution(&mut state, &result);

    assert_eq!(state.positions.len(), 1);
    let pos = &state.positions["MKT1"];
    assert_eq!(pos.market_id, "MKT1");
    assert_eq!(pos.direction, TradeDirection::Buy);
    assert!((pos.size - size_usd).abs() < 1e-9, "size={}", pos.size);
    assert!((pos.avg_price - 0.50).abs() < 1e-9);
    assert_eq!(pos.realized_pnl, 0.0);
    assert_eq!(pos.unrealized_pnl, 0.0);

    // Cash must decrease by the deployed amount.
    assert!(
        (state.cash - (BANKROLL - size_usd)).abs() < 1e-9,
        "cash={}", state.cash
    );
}

// ---------------------------------------------------------------------------
// 2. Unit: unfilled result is a no-op
// ---------------------------------------------------------------------------

#[test]
fn apply_execution_unfilled_is_noop() {
    let mut state  = fresh_state();
    let result     = make_unfilled_result("MKT1");

    PortfolioEngine::apply_execution(&mut state, &result);

    assert!(state.positions.is_empty());
    assert!((state.cash - BANKROLL).abs() < 1e-9);
    assert_eq!(state.total_realized_pnl, 0.0);
}

// ---------------------------------------------------------------------------
// 3. Unit: replacing a position crystallises realised PnL
// ---------------------------------------------------------------------------

#[test]
fn apply_execution_replaces_position_realises_pnl() {
    let mut state = fresh_state();

    // Open a Buy position at 0.40.
    let open = make_result("MKT1", TradeDirection::Buy, 0.05, 0.40, 0.40);
    PortfolioEngine::apply_execution(&mut state, &open);

    // Replace it — market_prob is now 0.60 → realised gain expected.
    let replace = make_result("MKT1", TradeDirection::Buy, 0.05, 0.60, 0.60);
    PortfolioEngine::apply_execution(&mut state, &replace);

    // Realised PnL: (0.60 − 0.40) × 500 = 100
    let expected_realised = (0.60 - 0.40) * (0.05 * BANKROLL);
    assert!(
        (state.total_realized_pnl - expected_realised).abs() < 1e-6,
        "total_realized_pnl={}, expected={}",
        state.total_realized_pnl, expected_realised
    );

    // Only one position should remain (the new one).
    assert_eq!(state.positions.len(), 1);
    assert!((state.positions["MKT1"].avg_price - 0.60).abs() < 1e-9);
}

// ---------------------------------------------------------------------------
// 4. Unit: sell direction realised PnL is directionally correct
// ---------------------------------------------------------------------------

#[test]
fn apply_execution_sell_direction_pnl() {
    let mut state = fresh_state();

    // Open a Sell position at 0.70.
    let open = make_result("MKT2", TradeDirection::Sell, 0.05, 0.70, 0.70);
    PortfolioEngine::apply_execution(&mut state, &open);

    // Replace with market_prob = 0.50 → Sell profits when price falls.
    let replace = make_result("MKT2", TradeDirection::Sell, 0.05, 0.50, 0.50);
    PortfolioEngine::apply_execution(&mut state, &replace);

    // Realised PnL for Sell: (entry − exit) × size = (0.70 − 0.50) × 500 = 100
    let expected_realised = (0.70 - 0.50) * (0.05 * BANKROLL);
    assert!(
        (state.total_realized_pnl - expected_realised).abs() < 1e-6,
        "total_realized_pnl={}", state.total_realized_pnl
    );
}

// ---------------------------------------------------------------------------
// 5. Unit: recalculate_unrealized — buy position profits when price rises
// ---------------------------------------------------------------------------

#[test]
fn recalculate_unrealized_buy_profit() {
    let mut state = fresh_state();

    // Buy at 0.40.
    let r = make_result("MKT1", TradeDirection::Buy, 0.05, 0.40, 0.40);
    PortfolioEngine::apply_execution(&mut state, &r);

    // Market moves to 0.65 → unrealised gain = (0.65 − 0.40) × 500 = 125
    let prices: HashMap<String, f64> = [("MKT1".to_string(), 0.65)].into();
    PortfolioEngine::recalculate_unrealized(&mut state, &prices);

    let expected = (0.65 - 0.40) * (0.05 * BANKROLL);
    assert!(
        (state.total_unrealized_pnl - expected).abs() < 1e-6,
        "total_unrealized_pnl={}", state.total_unrealized_pnl
    );
    assert!(
        (state.positions["MKT1"].unrealized_pnl - expected).abs() < 1e-6
    );
}

// ---------------------------------------------------------------------------
// 6. Unit: recalculate_unrealized — sell position profits when price falls
// ---------------------------------------------------------------------------

#[test]
fn recalculate_unrealized_sell_profit() {
    let mut state = fresh_state();

    // Sell at 0.70.
    let r = make_result("MKT1", TradeDirection::Sell, 0.05, 0.70, 0.70);
    PortfolioEngine::apply_execution(&mut state, &r);

    // Market falls to 0.50 → unrealised gain = (0.70 − 0.50) × 500 = 100
    let prices: HashMap<String, f64> = [("MKT1".to_string(), 0.50)].into();
    PortfolioEngine::recalculate_unrealized(&mut state, &prices);

    let expected = (0.70 - 0.50) * (0.05 * BANKROLL);
    assert!(
        (state.total_unrealized_pnl - expected).abs() < 1e-6,
        "total_unrealized_pnl={}", state.total_unrealized_pnl
    );
}

// ---------------------------------------------------------------------------
// 7. Unit: multiple positions — unrealised PnL is summed
// ---------------------------------------------------------------------------

#[test]
fn recalculate_unrealized_multiple_positions() {
    let mut state = fresh_state();

    PortfolioEngine::apply_execution(
        &mut state,
        &make_result("MKTA", TradeDirection::Buy, 0.05, 0.40, 0.40),
    );
    PortfolioEngine::apply_execution(
        &mut state,
        &make_result("MKTB", TradeDirection::Buy, 0.05, 0.55, 0.55),
    );

    let prices: HashMap<String, f64> = [
        ("MKTA".to_string(), 0.60), // gain 100
        ("MKTB".to_string(), 0.45), // loss  50
    ].into();
    PortfolioEngine::recalculate_unrealized(&mut state, &prices);

    let gain_a = (0.60 - 0.40) * (0.05 * BANKROLL); // 100
    let loss_b = (0.45 - 0.55) * (0.05 * BANKROLL); // -50
    let expected = gain_a + loss_b;

    assert!(
        (state.total_unrealized_pnl - expected).abs() < 1e-6,
        "total_unrealized_pnl={}, expected={}", state.total_unrealized_pnl, expected
    );
}

// ---------------------------------------------------------------------------
// 8. Unit: apply_risk_adjustments trims oversized positions
// ---------------------------------------------------------------------------

#[test]
fn apply_risk_adjustments_trims_oversized_position() {
    let mut state = fresh_state();

    // Deploy 10 % (oversized relative to 5 % cap).
    let r = make_result("MKT1", TradeDirection::Buy, 0.10, 0.50, 0.50);
    PortfolioEngine::apply_execution(&mut state, &r);

    let deployed_before = state.positions["MKT1"].size;
    assert!((deployed_before - 1_000.0).abs() < 1e-9);

    // Cap at 5 %.
    PortfolioEngine::apply_risk_adjustments(&mut state, 0.05);

    let max_size = BANKROLL * 0.05; // 500.0
    assert!(
        (state.positions["MKT1"].size - max_size).abs() < 1e-6,
        "size={}", state.positions["MKT1"].size
    );
    // Excess capital must be returned to cash.
    let expected_cash = BANKROLL - 0.10 * BANKROLL + (deployed_before - max_size);
    assert!(
        (state.cash - expected_cash).abs() < 1e-6,
        "cash={}, expected={}", state.cash, expected_cash
    );
}

// ---------------------------------------------------------------------------
// 9. Unit: apply_risk_adjustments leaves compliant positions untouched
// ---------------------------------------------------------------------------

#[test]
fn apply_risk_adjustments_leaves_small_positions_alone() {
    let mut state = fresh_state();

    let r = make_result("MKT1", TradeDirection::Buy, 0.03, 0.50, 0.50);
    PortfolioEngine::apply_execution(&mut state, &r);

    let size_before = state.positions["MKT1"].size;
    let cash_before = state.cash;

    PortfolioEngine::apply_risk_adjustments(&mut state, 0.05);

    assert!((state.positions["MKT1"].size - size_before).abs() < 1e-9);
    assert!((state.cash - cash_before).abs() < 1e-9);
}

// ---------------------------------------------------------------------------
// 10. Unit: total_equity accounts for realised and unrealised PnL
// ---------------------------------------------------------------------------

#[test]
fn total_equity_is_correct() {
    let mut state = fresh_state();

    // Open position, then price moves.
    let r = make_result("MKT1", TradeDirection::Buy, 0.05, 0.40, 0.40);
    PortfolioEngine::apply_execution(&mut state, &r);

    let prices: HashMap<String, f64> = [("MKT1".to_string(), 0.60)].into();
    PortfolioEngine::recalculate_unrealized(&mut state, &prices);

    // equity = bankroll + 0 realised + 100 unrealised = 10_100
    let expected = BANKROLL + (0.60 - 0.40) * (0.05 * BANKROLL);
    assert!(
        (state.total_equity() - expected).abs() < 1e-6,
        "equity={}", state.total_equity()
    );
}

// ---------------------------------------------------------------------------
// 11. Integration: Execution event produces a Portfolio event
// ---------------------------------------------------------------------------

#[tokio::test]
async fn execution_event_produces_portfolio_update() {
    let bus    = EventBus::new();
    let mut rx = bus.subscribe();

    let cancel = spawn_engine(&bus, PortfolioConfig::default());

    let result = make_result("BTCUSD", TradeDirection::Buy, 0.05, 0.50, 0.50);
    bus.publish(Event::Execution(result)).unwrap();

    // Collect the first Portfolio event.
    let portfolio_update = timeout(Duration::from_millis(300), async {
        loop {
            if let Ok(ev) = rx.recv().await {
                if let Event::Portfolio(pu) = ev.as_ref() {
                    return Some(pu.clone());
                }
            }
        }
    })
    .await
    .expect("timed out waiting for Portfolio event")
    .expect("no Portfolio event received");

    cancel.cancel();

    assert_eq!(portfolio_update.portfolio.positions.len(), 1);
    assert_eq!(portfolio_update.portfolio.positions[0].market_id, "BTCUSD");
    assert!(portfolio_update.portfolio.exposure > 0.0);
}

// ---------------------------------------------------------------------------
// 12. Integration: two executions in different markets → two positions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_executions_two_positions() {
    let bus    = EventBus::new();
    let mut rx = bus.subscribe();
    let cancel = spawn_engine(&bus, PortfolioConfig::default());

    bus.publish(Event::Execution(make_result("MKT1", TradeDirection::Buy,  0.05, 0.40, 0.40))).unwrap();
    bus.publish(Event::Execution(make_result("MKT2", TradeDirection::Sell, 0.07, 0.70, 0.70))).unwrap();

    // Collect two Portfolio events; take the last one.
    let mut last = None;
    let _ = timeout(Duration::from_millis(400), async {
        let mut count = 0;
        loop {
            if let Ok(ev) = rx.recv().await {
                if let Event::Portfolio(pu) = ev.as_ref() {
                    last  = Some(pu.clone());
                    count += 1;
                    if count >= 2 { return; }
                }
            }
        }
    })
    .await;

    cancel.cancel();

    let pu = last.expect("expected at least one Portfolio event");
    assert_eq!(pu.portfolio.positions.len(), 2, "expected 2 positions");
}

// ---------------------------------------------------------------------------
// 13. Integration: unfilled execution does not change portfolio
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unfilled_execution_does_not_create_position() {
    let bus    = EventBus::new();
    let engine = PortfolioEngine::new(PortfolioConfig::default(), bus.clone());
    let state  = engine.get_portfolio_state();
    let cancel = {
        let c = CancellationToken::new();
        let cc = c.clone();
        tokio::spawn(async move { engine.run(cc).await });
        c
    };

    let unfilled = make_unfilled_result("MKTU");
    bus.publish(Event::Execution(unfilled)).unwrap();

    // Give the engine time to process (it should do nothing).
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel.cancel();

    let s = state.read().await;
    assert!(s.positions.is_empty(), "unfilled should produce no position");
    assert!((s.cash - BANKROLL).abs() < 1e-9, "cash should be unchanged");
}

// ---------------------------------------------------------------------------
// 14. Integration: partial fill deploys proportional capital
// ---------------------------------------------------------------------------

#[tokio::test]
async fn partial_fill_deploys_correct_capital() {
    let bus    = EventBus::new();
    let engine = PortfolioEngine::new(PortfolioConfig::default(), bus.clone());
    let state  = engine.get_portfolio_state();
    let cancel = {
        let c = CancellationToken::new();
        let cc = c.clone();
        tokio::spawn(async move { engine.run(cc).await });
        c
    };

    // Simulate a 50 % partial fill on a 0.10 request → 0.05 executed.
    let trade = ApprovedTrade {
        market_id:         "MKTPART".to_string(),
        direction:         TradeDirection::Buy,
        approved_fraction: 0.10,
        expected_value:    0.05,
        posterior_prob:    0.65,
        market_prob:       0.55,
        signal_source:     "test".to_string(),
        signal_timestamp:  Utc::now(),
        timestamp:         Utc::now(),
    };
    let result = ExecutionResult {
        trade,
        filled:             true,
        fill_ratio:         0.5,
        executed_quantity:  0.05, // only 50 % filled
        avg_price:          0.55,
        slippage:           0.0,
        timestamp:          Utc::now(),
    };

    bus.publish(Event::Execution(result)).unwrap();

    tokio::time::sleep(Duration::from_millis(150)).await;
    cancel.cancel();

    let s = state.read().await;
    let pos = s.positions.get("MKTPART").expect("position should exist");
    let expected_size = 0.05 * BANKROLL; // 500 USD
    assert!(
        (pos.size - expected_size).abs() < 1e-6,
        "pos.size={}, expected={}", pos.size, expected_size
    );
}
