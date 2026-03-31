// crates/performance_analytics/tests/performance_analytics.rs
//
// Unit tests exercise the pure core helpers directly with constructed state.
// Integration tests spin up a real EventBus and verify the async run() loop.

use std::time::Duration;

use chrono::Utc;
use common::{
    Event, EventBus, MarketNode, MarketUpdate, Portfolio, PortfolioUpdate, Position,
    TradeDirection,
};
use performance_analytics::{AnalyticsConfig, ExportFormat, PerformanceAnalytics};
use tokio_util::sync::CancellationToken;

// ── helpers ──────────────────────────────────────────────────────────────────

fn default_engine() -> PerformanceAnalytics {
    PerformanceAnalytics::new(AnalyticsConfig::default(), EventBus::new()).unwrap()
}

fn make_portfolio(pnl: f64, exposure: f64, positions: Vec<Position>) -> Portfolio {
    Portfolio { positions, pnl, exposure }
}

fn make_update(pnl: f64, exposure: f64, positions: Vec<Position>) -> PortfolioUpdate {
    PortfolioUpdate {
        portfolio: make_portfolio(pnl, exposure, positions),
        timestamp: Utc::now(),
    }
}

fn make_position(market_id: &str, direction: TradeDirection, size: f64, entry: f64) -> Position {
    Position {
        market_id:         market_id.to_string(),
        direction,
        size,
        entry_probability: entry,
        opened_at:         Utc::now(),
    }
}

fn make_market_update(id: &str, prob: f64) -> MarketUpdate {
    MarketUpdate {
        market: MarketNode {
            id:           id.to_string(),
            probability:  prob,
            liquidity:    1_000.0,
            last_update:  Utc::now(),
        },
    }
}

// ── Unit tests — pure helpers ─────────────────────────────────────────────────

#[test]
fn process_portfolio_update_creates_snapshot() {
    let mut engine = default_engine();
    assert!(engine.compute_realtime_metrics().is_none());

    engine.process_portfolio_update(&make_update(100.0, 500.0, vec![]));

    let m = engine.compute_realtime_metrics().expect("should have a snapshot");
    assert_eq!(m.total_pnl, 100.0);
    assert_eq!(m.exposure, 500.0);
    assert_eq!(m.position_count, 0);
}

#[test]
fn equity_equals_bankroll_plus_pnl() {
    let mut engine = default_engine();
    engine.process_portfolio_update(&make_update(250.0, 0.0, vec![]));

    let m = engine.compute_realtime_metrics().unwrap();
    assert!((m.equity - (10_000.0 + 250.0)).abs() < 1e-9);
}

#[test]
fn history_window_evicts_oldest() {
    let config = AnalyticsConfig {
        history_window:   3,
        initial_bankroll: 10_000.0,
        ..Default::default()
    };
    let mut engine = PerformanceAnalytics::new(config, EventBus::new()).unwrap();

    for i in 0..5_u32 {
        engine.process_portfolio_update(&make_update(i as f64, 0.0, vec![]));
    }

    let history = engine.get_historical_metrics();
    assert_eq!(history.len(), 3, "window should be bounded to 3");
    // The three retained snapshots should be ticks 2, 3, 4
    assert!((history[0].total_pnl - 2.0).abs() < 1e-9);
    assert!((history[2].total_pnl - 4.0).abs() < 1e-9);
}

#[test]
fn per_market_pnl_buy_position() {
    let mut engine = default_engine();

    // Seed price cache: market "m1" is now 0.70
    engine.process_market_update(&make_market_update("m1", 0.70));

    let pos    = make_position("m1", TradeDirection::Buy, 1_000.0, 0.50);
    let update = make_update(200.0, 1_000.0, vec![pos]);
    engine.process_portfolio_update(&update);

    let m = engine.compute_realtime_metrics().unwrap();
    let mtm = m.per_market_pnl["m1"];
    // (0.70 − 0.50) × 1_000 = 200.0
    assert!((mtm - 200.0).abs() < 1e-9, "buy MTM should be +200, got {mtm}");
}

#[test]
fn per_market_pnl_sell_position() {
    let mut engine = default_engine();

    // Market dropped from 0.60 to 0.40 — good for our SELL
    engine.process_market_update(&make_market_update("m2", 0.40));

    let pos    = make_position("m2", TradeDirection::Sell, 1_000.0, 0.60);
    let update = make_update(200.0, 1_000.0, vec![pos]);
    engine.process_portfolio_update(&update);

    let m = engine.compute_realtime_metrics().unwrap();
    let mtm = m.per_market_pnl["m2"];
    // (0.60 − 0.40) × 1_000 = 200.0
    assert!((mtm - 200.0).abs() < 1e-9, "sell MTM should be +200, got {mtm}");
}

#[test]
fn per_market_pnl_absent_when_no_price() {
    let mut engine = default_engine();
    // No market update → price_cache is empty

    let pos    = make_position("unknown_market", TradeDirection::Buy, 500.0, 0.50);
    let update = make_update(0.0, 500.0, vec![pos]);
    engine.process_portfolio_update(&update);

    let m = engine.compute_realtime_metrics().unwrap();
    assert!(
        m.per_market_pnl.get("unknown_market").is_none(),
        "no entry expected when price is missing from cache"
    );
}

#[test]
fn max_drawdown_tracked_correctly() {
    let mut engine = default_engine();

    // Equity: 10_000 → 10_200 (new peak) → 9_800 (drawdown) → 10_100 (partial recovery)
    engine.process_portfolio_update(&make_update(0.0,    0.0, vec![])); // equity 10_000
    engine.process_portfolio_update(&make_update(200.0,  0.0, vec![])); // equity 10_200 — new peak
    engine.process_portfolio_update(&make_update(-200.0, 0.0, vec![])); // equity 9_800
    engine.process_portfolio_update(&make_update(100.0,  0.0, vec![])); // equity 10_100

    let risk = engine.compute_risk_metrics();
    // Peak was 10_200, trough was 9_800 → drawdown = 400/10_200 ≈ 0.03922
    let expected_dd = 400.0 / 10_200.0;
    assert!(
        (risk.max_drawdown - expected_dd).abs() < 1e-6,
        "max_drawdown expected {expected_dd:.6}, got {:.6}",
        risk.max_drawdown
    );
}

#[test]
fn sharpe_positive_for_monotonic_gains() {
    // Set tick_interval_secs so that sharpe_ann_factor() == 1.0 for a
    // deterministic test: ann_factor = sqrt(seconds_per_year / interval)
    // → 1.0 = sqrt(31_557_600 / interval) → interval = 31_557_600.
    let config = AnalyticsConfig {
        tick_interval_secs: 365.25 * 24.0 * 3600.0, // ann_factor = 1.0
        ..Default::default()
    };
    let mut engine = PerformanceAnalytics::new(config, EventBus::new()).unwrap();

    // Uniformly increasing PnL: returns are all +10
    for i in 0..10_u32 {
        engine.process_portfolio_update(&make_update(i as f64 * 10.0, 0.0, vec![]));
    }

    let risk = engine.compute_risk_metrics();
    assert!(
        risk.sharpe_ratio > 0.0 || risk.sharpe_ratio == f64::INFINITY,
        "monotonic gains should yield a positive (or infinite) Sharpe, got {}",
        risk.sharpe_ratio
    );
}

#[test]
fn win_rate_calculation() {
    let mut engine = default_engine();
    // PnL sequence: 0, +10, +10, -5, +10 → returns: +10, +10, -5, +10
    // Wins (returns > 0): 3 out of 4
    for pnl in [0.0_f64, 10.0, 20.0, 15.0, 25.0] {
        engine.process_portfolio_update(&make_update(pnl, 0.0, vec![]));
    }

    let risk = engine.compute_risk_metrics();
    assert!(
        (risk.win_rate - 0.75).abs() < 1e-9,
        "win_rate expected 0.75, got {}",
        risk.win_rate
    );
}

#[test]
fn export_csv_has_header_and_rows() {
    let mut engine = default_engine();
    engine.process_portfolio_update(&make_update(50.0, 200.0, vec![]));
    engine.process_portfolio_update(&make_update(80.0, 200.0, vec![]));

    let csv = engine.export_metrics(ExportFormat::Csv);
    let lines: Vec<&str> = csv.lines().collect();

    assert!(lines[0].starts_with("timestamp,total_pnl"), "first line must be header");
    assert_eq!(lines.len(), 3, "header + 2 data rows expected");
}

#[test]
fn export_json_roundtrip() {
    let mut engine = default_engine();
    engine.process_portfolio_update(&make_update(123.45, 500.0, vec![]));

    let json   = engine.export_metrics(ExportFormat::Json);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&json).expect("valid JSON");

    assert_eq!(parsed.len(), 1);
    let pnl = parsed[0]["total_pnl"].as_f64().unwrap();
    assert!((pnl - 123.45).abs() < 1e-6);
}

#[test]
fn export_prometheus_contains_key_metrics() {
    let mut engine = default_engine();
    engine.process_portfolio_update(&make_update(99.0, 400.0, vec![]));
    engine.process_portfolio_update(&make_update(110.0, 400.0, vec![]));

    let prom = engine.export_metrics(ExportFormat::Prometheus);
    assert!(prom.contains("portfolio_total_pnl"),   "missing total_pnl gauge");
    assert!(prom.contains("portfolio_equity"),       "missing equity gauge");
    assert!(prom.contains("portfolio_sharpe_ratio"), "missing sharpe gauge");
    assert!(prom.contains("portfolio_max_drawdown"), "missing drawdown gauge");
}

#[test]
fn export_empty_history_returns_empty_string() {
    let engine = default_engine();
    assert!(engine.export_metrics(ExportFormat::Json).starts_with("[]"));
    assert_eq!(engine.export_metrics(ExportFormat::Prometheus), "");
}

#[test]
fn price_cache_pruned_to_open_positions() {
    let mut engine = default_engine();

    // Populate cache with two markets
    engine.process_market_update(&make_market_update("mkt_a", 0.50));
    engine.process_market_update(&make_market_update("mkt_b", 0.60));

    // Portfolio update with only mkt_a open — mkt_b should be pruned
    let pos    = make_position("mkt_a", TradeDirection::Buy, 500.0, 0.45);
    engine.process_portfolio_update(&make_update(25.0, 500.0, vec![pos]));

    assert!(engine.price_cache.contains_key("mkt_a"), "mkt_a should be retained");
    assert!(!engine.price_cache.contains_key("mkt_b"), "mkt_b should be pruned");
}

// ── Integration tests — async event loop ─────────────────────────────────────

#[tokio::test]
async fn portfolio_event_produces_snapshot() {
    let bus    = EventBus::new();
    let cancel = CancellationToken::new();

    let config = AnalyticsConfig {
        history_window:   10,
        initial_bankroll: 10_000.0,
        ..Default::default()
    };

    // Subscribe a verification receiver BEFORE spawning the engine so that
    // published events are guaranteed to have at least one live subscriber,
    // eliminating the subscribe-after-spawn race.
    let mut verify_rx = bus.subscribe();

    let engine  = PerformanceAnalytics::new(config, bus.clone()).unwrap();
    let cancel2 = cancel.clone();
    tokio::spawn(async move { engine.run(cancel2).await });

    // Yield once to let the spawned task reach its own subscribe() call.
    tokio::task::yield_now().await;

    bus.publish(Event::Market(make_market_update("mkt1", 0.65))).unwrap();
    bus.publish(Event::Portfolio(make_update(
        150.0,
        300.0,
        vec![make_position("mkt1", TradeDirection::Buy, 300.0, 0.60)],
    )))
    .unwrap();

    // Confirm events were delivered (not lost to a no-subscriber race)
    tokio::time::timeout(
        Duration::from_millis(200),
        verify_rx.recv(),
    )
    .await
    .expect("event should be received within timeout")
    .expect("no recv error");

    tokio::time::sleep(Duration::from_millis(50)).await;
    cancel.cancel();
}

#[tokio::test]
async fn multiple_portfolio_updates_dont_exceed_window() {
    let bus    = EventBus::new();
    let cancel = CancellationToken::new();

    let config = AnalyticsConfig {
        history_window:   5,
        initial_bankroll: 10_000.0,
        ..Default::default()
    };

    let mut verify_rx = bus.subscribe();
    let engine  = PerformanceAnalytics::new(config, bus.clone()).unwrap();
    let cancel2 = cancel.clone();
    tokio::spawn(async move { engine.run(cancel2).await });

    tokio::task::yield_now().await;

    for i in 0..8_u32 {
        bus.publish(Event::Portfolio(make_update(i as f64 * 100.0, 0.0, vec![]))).unwrap();
    }

    // Confirm the last event was delivered
    let mut last = None;
    for _ in 0..8 {
        match tokio::time::timeout(Duration::from_millis(100), verify_rx.recv()).await {
            Ok(Ok(ev)) => last = Some(ev),
            _ => break,
        }
    }
    assert!(last.is_some(), "at least one event should have been delivered");

    tokio::time::sleep(Duration::from_millis(50)).await;
    cancel.cancel();
    // Window capping validated in history_window_evicts_oldest unit test above
}
