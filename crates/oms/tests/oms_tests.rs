// crates/oms/tests/oms_tests.rs
//
// Integration tests for the Order Management System.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use common::events::{ApprovedTrade, Event};
use common::event_bus::EventBus;
use common::types::TradeDirection;
use exchange_api::{
    CancelResult, ExchangeConnector, ExchangeError, ExchangeFill, ExchangeOrder,
    ExchangeOrderBook, OrderId, OrderRequest, OrderSide, OrderStatus, OrderType,
};
use exchange_api::types::ExchangePosition;
use oms::manager::OmsConfig;
use oms::state::{ManagedOrder, OrderManagerState};
use oms::OrderManager;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

// ── MockExchange ────────────────────────────────────────────────────────────

struct MockExchange {
    name: String,
}

impl MockExchange {
    fn new(name: &str) -> Self {
        Self { name: name.to_string() }
    }
}

#[async_trait]
impl ExchangeConnector for MockExchange {
    fn name(&self) -> &str {
        &self.name
    }

    async fn get_balance(&self) -> Result<f64, ExchangeError> {
        Ok(10_000.0)
    }

    async fn get_positions(&self) -> Result<Vec<ExchangePosition>, ExchangeError> {
        Ok(vec![])
    }

    async fn place_order(&self, req: &OrderRequest) -> Result<OrderId, ExchangeError> {
        Ok(OrderId(format!("MOCK-{}", req.market_id)))
    }

    async fn cancel_order(&self, order_id: &OrderId) -> Result<CancelResult, ExchangeError> {
        Ok(CancelResult {
            order_id: order_id.clone(),
            success: true,
            filled_before_cancel_usd: 0.0,
        })
    }

    async fn get_order_status(&self, order_id: &OrderId) -> Result<ExchangeOrder, ExchangeError> {
        Ok(ExchangeOrder {
            order_id: order_id.clone(),
            market_id: "test".to_string(),
            side: OrderSide::BuyYes,
            order_type: OrderType::Limit,
            status: OrderStatus::Filled,
            requested_size_usd: 100.0,
            filled_size_usd: 100.0,
            avg_fill_price: Some(0.55),
            limit_price: Some(0.55),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        })
    }

    async fn get_open_orders(&self) -> Result<Vec<ExchangeOrder>, ExchangeError> {
        Ok(vec![])
    }

    async fn get_order_book(&self, _market_id: &str) -> Result<ExchangeOrderBook, ExchangeError> {
        Ok(ExchangeOrderBook {
            market_id: "test".to_string(),
            bids: vec![],
            asks: vec![],
            timestamp: Utc::now(),
        })
    }

    async fn get_recent_fills(
        &self,
        _market_id: &str,
        _limit: usize,
    ) -> Result<Vec<ExchangeFill>, ExchangeError> {
        Ok(vec![])
    }

    fn fee_rate(&self) -> f64 {
        0.02
    }

    fn min_order_size_usd(&self) -> f64 {
        1.0
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn make_approved_trade(market_id: &str, fraction: f64) -> ApprovedTrade {
    ApprovedTrade {
        market_id: market_id.to_string(),
        direction: TradeDirection::Buy,
        approved_fraction: fraction,
        expected_value: 0.05,
        posterior_prob: 0.6,
        market_prob: 0.55,
        signal_source: "test".to_string(),
        signal_timestamp: Utc::now(),
        timestamp: Utc::now(),
    }
}

fn make_order_request(market_id: &str, size_usd: f64) -> OrderRequest {
    OrderRequest {
        market_id: market_id.to_string(),
        side: OrderSide::BuyYes,
        order_type: OrderType::Limit,
        size_usd,
        price: Some(0.55),
        signal_source: "test".to_string(),
        signal_timestamp: Utc::now(),
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn dry_run_mode_synthesizes_fill() {
    let bus = EventBus::new();
    let mut rx = bus.subscribe();

    let config = OmsConfig {
        live_trading_enabled: false,
        bankroll: 10_000.0,
        ..OmsConfig::default()
    };

    let oms = OrderManager::new(config, bus.clone())
        .with_exchange(Arc::new(MockExchange::new("polymarket")));

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    // Spawn the OMS event loop.
    let handle = tokio::spawn(async move {
        oms.run(cancel_clone).await;
    });

    // Give the OMS a moment to start, then publish an ApprovedTrade.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let trade = make_approved_trade("poly-market-abc", 0.05);
    bus.publish(Event::ApprovedTrade(trade)).unwrap();

    // Collect events: we expect at least an EdgeDecayReport and an Execution.
    let mut found_execution = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);

    while tokio::time::Instant::now() < deadline {
        match timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Ok(ev)) => {
                if let Event::Execution(exec) = ev.as_ref() {
                    assert!(exec.filled, "dry-run should synthesize a fill");
                    assert!((exec.fill_ratio - 1.0).abs() < f64::EPSILON);
                    assert_eq!(exec.trade.market_id, "poly-market-abc");
                    found_execution = true;
                    break;
                }
            }
            _ => break,
        }
    }

    cancel.cancel();
    let _ = timeout(Duration::from_secs(2), handle).await;
    assert!(found_execution, "should have received an Execution event");
}

#[tokio::test]
async fn order_state_lifecycle() {
    let mut state = OrderManagerState::default();

    // 1) Insert order.
    let req = make_order_request("MKT-LIFE", 100.0);
    let oms_id = state.insert_order(req, "polymarket", TradeDirection::Buy);
    assert_eq!(oms_id, 0);
    assert_eq!(state.total_submitted, 1);

    let order = state.orders.get(&oms_id).unwrap();
    assert_eq!(order.status, OrderStatus::Pending);
    assert!(order.exchange_order_id.is_none());

    // 2) Set exchange ID.
    state.set_exchange_id(oms_id, OrderId("EX-123".to_string()));
    let order = state.orders.get(&oms_id).unwrap();
    assert_eq!(order.status, OrderStatus::Open);
    assert_eq!(
        order.exchange_order_id.as_ref().unwrap().0,
        "EX-123"
    );

    // 3) Record fill.
    state.record_fill(oms_id, 100.0, 0.55, true);
    let order = state.orders.get(&oms_id).unwrap();
    assert_eq!(order.status, OrderStatus::Filled);
    assert_eq!(order.filled_size_usd, 100.0);
    assert_eq!(order.avg_fill_price, Some(0.55));
    assert_eq!(state.total_filled, 1);
    assert!((state.total_volume_usd - 100.0).abs() < f64::EPSILON);
}

#[tokio::test]
async fn exchange_routing_kalshi() {
    let bus = EventBus::new();
    let _rx = bus.subscribe();

    let config = OmsConfig {
        live_trading_enabled: false,
        bankroll: 10_000.0,
        ..OmsConfig::default()
    };

    let oms = OrderManager::new(config, bus.clone())
        .with_exchange(Arc::new(MockExchange::new("kalshi")))
        .with_exchange(Arc::new(MockExchange::new("polymarket")));

    let oms_state = oms.state();
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    let handle = tokio::spawn(async move {
        oms.run(cancel_clone).await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // KALSHI- prefix should route to kalshi connector.
    let trade = make_approved_trade("KALSHI-PRES-2024", 0.05);
    bus.publish(Event::ApprovedTrade(trade)).unwrap();

    // Wait for the OMS to process.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let state = oms_state.read().await;
    assert_eq!(state.total_submitted, 1);
    let order = state.orders.get(&0).unwrap();
    assert_eq!(order.exchange, "kalshi");

    drop(state);
    cancel.cancel();
    let _ = timeout(Duration::from_secs(2), handle).await;
}

#[tokio::test]
async fn exchange_routing_polymarket() {
    let bus = EventBus::new();
    let _rx = bus.subscribe();

    let config = OmsConfig {
        live_trading_enabled: false,
        bankroll: 10_000.0,
        ..OmsConfig::default()
    };

    let oms = OrderManager::new(config, bus.clone())
        .with_exchange(Arc::new(MockExchange::new("kalshi")))
        .with_exchange(Arc::new(MockExchange::new("polymarket")));

    let oms_state = oms.state();
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    let handle = tokio::spawn(async move {
        oms.run(cancel_clone).await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // A lowercase market ID should route to polymarket.
    let trade = make_approved_trade("poly-market-xyz", 0.05);
    bus.publish(Event::ApprovedTrade(trade)).unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let state = oms_state.read().await;
    assert_eq!(state.total_submitted, 1);
    let order = state.orders.get(&0).unwrap();
    assert_eq!(order.exchange, "polymarket");

    drop(state);
    cancel.cancel();
    let _ = timeout(Duration::from_secs(2), handle).await;
}

#[tokio::test]
async fn managed_order_is_terminal() {
    let mut state = OrderManagerState::default();

    // Helper to create an order and set it to the given status.
    let mut check_terminal = |status: OrderStatus, expected: bool| {
        let req = make_order_request("MKT-TERM", 100.0);
        let oms_id = state.insert_order(req, "test", TradeDirection::Buy);
        let order = state.orders.get_mut(&oms_id).unwrap();
        order.status = status;
        assert_eq!(
            order.is_terminal(),
            expected,
            "OrderStatus::{:?} terminal={} expected={}",
            status,
            order.is_terminal(),
            expected
        );
    };

    // Terminal states.
    check_terminal(OrderStatus::Filled, true);
    check_terminal(OrderStatus::Cancelled, true);
    check_terminal(OrderStatus::Rejected, true);
    check_terminal(OrderStatus::Expired, true);

    // Non-terminal states.
    check_terminal(OrderStatus::Pending, false);
    check_terminal(OrderStatus::Open, false);
    check_terminal(OrderStatus::PartiallyFilled, false);
}

#[tokio::test]
async fn fill_ratio_calculation() {
    let mut state = OrderManagerState::default();

    // Normal case: partial fill.
    let req = make_order_request("MKT-RATIO", 200.0);
    let oms_id = state.insert_order(req, "test", TradeDirection::Buy);
    state.record_fill(oms_id, 100.0, 0.55, false);
    let order = state.orders.get(&oms_id).unwrap();
    assert!((order.fill_ratio() - 0.5).abs() < f64::EPSILON, "50% fill");

    // Full fill.
    state.record_fill(oms_id, 200.0, 0.55, true);
    let order = state.orders.get(&oms_id).unwrap();
    assert!((order.fill_ratio() - 1.0).abs() < f64::EPSILON, "100% fill");

    // Zero-size edge case: should return 0.0 without panic.
    let req_zero = make_order_request("MKT-ZERO", 0.0);
    let oms_id_zero = state.insert_order(req_zero, "test", TradeDirection::Buy);
    let order_zero = state.orders.get(&oms_id_zero).unwrap();
    assert!(
        (order_zero.fill_ratio() - 0.0).abs() < f64::EPSILON,
        "zero-size order should have fill_ratio 0.0"
    );

    // Overfill clamp: filled_size > request size should clamp to 1.0.
    let req_over = make_order_request("MKT-OVER", 50.0);
    let oms_id_over = state.insert_order(req_over, "test", TradeDirection::Buy);
    state.record_fill(oms_id_over, 100.0, 0.55, true);
    let order_over = state.orders.get(&oms_id_over).unwrap();
    assert!(
        (order_over.fill_ratio() - 1.0).abs() < f64::EPSILON,
        "overfill should clamp to 1.0"
    );
}
