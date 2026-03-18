// crates/simulation_engine/tests/simulation_tests.rs
//
// Integration tests for the Simulation Engine.
//
// These tests exercise the public API end-to-end without mocking internals.
// All tests that do not require async use plain `#[test]`; tests that need
// the Tokio runtime use `#[tokio::test]`.

use common::EventBus;
use simulation_engine::{
    config::{MarketSpec, MonteCarloConfig, ParameterSweepPoint, SimulationConfig},
    generators::MarketGenerator,
    metrics::SimMetricsCollector,
    replay::HistoricalReplayer,
    types::SimulationMode,
    SimulationEngine,
};
use std::path::Path;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build a minimal `SimulationConfig` suitable for fast unit tests.
fn fast_config() -> SimulationConfig {
    SimulationConfig {
        mode:              SimulationMode::MonteCarlo,
        tick_interval_ms:  0,          // no sleep in tests
        initial_capital:   1_000.0,
        max_ticks:         50,
        monte_carlo:       MonteCarloConfig {
            runs:        5,
            volatility:  0.02,
            drift:       0.0,
            seed:        Some(42),     // deterministic
            correlation: 0.0,
        },
        markets: vec![
            MarketSpec { id: "T-A".to_string(), initial_prob: 0.50, liquidity: 1_000.0 },
            MarketSpec { id: "T-B".to_string(), initial_prob: 0.45, liquidity: 1_000.0 },
        ],
        signal_threshold:  0.05,
        position_fraction: 0.05,
        ..SimulationConfig::default()
    }
}

fn three_market_specs() -> Vec<MarketSpec> {
    vec![
        MarketSpec { id: "M1".to_string(), initial_prob: 0.50, liquidity: 5_000.0 },
        MarketSpec { id: "M2".to_string(), initial_prob: 0.30, liquidity: 3_000.0 },
        MarketSpec { id: "M3".to_string(), initial_prob: 0.70, liquidity: 2_000.0 },
    ]
}

// ── Test 1: default config passes validation ──────────────────────────────────

#[test]
fn test_default_config_is_valid() {
    assert!(
        SimulationConfig::default().validate().is_ok(),
        "default SimulationConfig must be valid"
    );
}

// ── Test 2: config validation catches invalid capital ─────────────────────────

#[test]
fn test_config_rejects_zero_capital() {
    let cfg = SimulationConfig {
        initial_capital: 0.0,
        ..SimulationConfig::default()
    };
    assert!(
        cfg.validate().is_err(),
        "zero initial_capital must fail validation"
    );
}

// ── Test 3: config validation catches invalid position_fraction ───────────────

#[test]
fn test_config_rejects_out_of_range_position_fraction() {
    let cfg = SimulationConfig {
        position_fraction: 1.5,
        ..SimulationConfig::default()
    };
    assert!(cfg.validate().is_err());
}

// ── Test 4: random walk stays bounded in [0.01, 0.99] ────────────────────────

#[test]
fn test_random_walk_probability_bounds() {
    let mc_cfg = MonteCarloConfig {
        volatility: 0.10, // high volatility to stress the clamping
        seed: Some(1234),
        ..MonteCarloConfig::default()
    };
    let mut gen = MarketGenerator::new(three_market_specs(), mc_cfg);

    for _ in 0..500 {
        let tick = gen.next_tick();
        for upd in &tick.market_updates {
            let p = upd.market.probability;
            assert!(
                p >= 0.01 && p <= 0.99,
                "probability {p} out of [0.01, 0.99] bounds"
            );
        }
    }
}

// ── Test 5: positive drift increases probabilities on average ─────────────────

#[test]
fn test_random_walk_positive_drift_increases_mean_probability() {
    let mc_cfg = MonteCarloConfig {
        volatility: 0.005,
        drift:      0.01, // strong upward drift
        seed:       Some(7),
        correlation: 0.0,
        ..MonteCarloConfig::default()
    };
    let specs = vec![
        MarketSpec { id: "D".to_string(), initial_prob: 0.40, liquidity: 1_000.0 },
    ];
    let mut gen = MarketGenerator::new(specs, mc_cfg);

    let mut prob_sum = 0.0;
    let n = 200usize;
    for _ in 0..n {
        let tick = gen.next_tick();
        prob_sum += tick.market_updates[0].market.probability;
    }
    let mean = prob_sum / n as f64;
    // With drift=0.01 per tick and 200 ticks starting at 0.40, mean should
    // be clearly above 0.40 (clamping at 0.99 pulls it down, so we use a
    // conservative threshold).
    assert!(
        mean > 0.42,
        "expected mean > 0.42 with positive drift, got {mean:.4}"
    );
}

// ── Test 6: correlated markets move in the same direction more often ──────────

#[test]
fn test_correlated_markets_move_together() {
    let mc_cfg = MonteCarloConfig {
        volatility:  0.02,
        drift:       0.0,
        seed:        Some(99),
        correlation: 0.9, // strong positive correlation
        runs:        1,
    };
    let specs = vec![
        MarketSpec { id: "C1".to_string(), initial_prob: 0.50, liquidity: 1_000.0 },
        MarketSpec { id: "C2".to_string(), initial_prob: 0.50, liquidity: 1_000.0 },
    ];
    let mut gen = MarketGenerator::new(specs.clone(), mc_cfg.clone());

    let n = 400usize;
    let mut same_direction = 0usize;
    let mut prev = (0.50f64, 0.50f64);

    for _ in 0..n {
        let tick = gen.next_tick();
        let p0 = tick.market_updates[0].market.probability;
        let p1 = tick.market_updates[1].market.probability;
        let d0 = p0 - prev.0;
        let d1 = p1 - prev.1;
        if d0 * d1 > 0.0 {
            same_direction += 1;
        }
        prev = (p0, p1);
    }

    // With correlation=0.9 the two markets should move together >70% of ticks.
    let fraction = same_direction as f64 / n as f64;
    assert!(
        fraction > 0.65,
        "expected >65% same-direction moves with corr=0.9, got {:.1}%",
        fraction * 100.0
    );
}

// ── Test 7: Monte Carlo returns the configured number of results ──────────────

#[test]
fn test_monte_carlo_run_count() {
    let bus = EventBus::new();
    let cfg = fast_config(); // runs = 5
    let mut engine = SimulationEngine::new(cfg, bus);
    let results = engine.run_monte_carlo();
    assert_eq!(results.len(), 5, "expected 5 simulation results");
}

// ── Test 8: MC results are accumulated in collect_results() ──────────────────

#[test]
fn test_collect_results_accumulates_across_calls() {
    let bus = EventBus::new();
    let cfg = fast_config();
    let mut engine = SimulationEngine::new(cfg, bus);
    engine.run_monte_carlo();
    engine.run_monte_carlo();
    assert_eq!(
        engine.collect_results().len(),
        10,
        "two batches of 5 runs should give 10 accumulated results"
    );
}

// ── Test 9: metrics win rate is computed correctly ────────────────────────────

#[test]
fn test_metrics_win_rate() {
    use common::{
        ApprovedTrade, ExecutionResult, TradeDirection,
    };
    use chrono::Utc;

    let mut collector = SimMetricsCollector::new();

    // Simulate 3 winning and 1 losing execution.
    let now = Utc::now();
    let approved = ApprovedTrade {
        market_id:         "X".to_string(),
        direction:         TradeDirection::Buy,
        approved_fraction: 0.05,
        expected_value:    0.1,
        posterior_prob:    0.6,
        market_prob:       0.5,
        signal_timestamp:  now,
        timestamp:         now,
    };

    // 3 filled executions with negative slippage (profitable fills → pnl_proxy > 0)
    for _ in 0..3 {
        collector.on_execution(&ExecutionResult {
            trade:             approved.clone(),
            filled:            true,
            fill_ratio:        1.0,
            executed_quantity: 0.05,
            avg_price:         0.50,
            slippage:          -0.01, // negative slippage → pnl_proxy = 0.01*0.05 > 0
            timestamp:         now,
        });
    }
    // 1 filled execution with positive slippage (losing fill → pnl_proxy < 0)
    collector.on_execution(&ExecutionResult {
        trade:             approved.clone(),
        filled:            true,
        fill_ratio:        1.0,
        executed_quantity: 0.05,
        avg_price:         0.52,
        slippage:          0.05,      // positive slippage → pnl_proxy = -0.0025 < 0
        timestamp:         now,
    });

    let result = collector.compute_result(0, 1_000.0);
    assert!(
        (result.win_rate - 0.75).abs() < 1e-9,
        "expected win_rate = 0.75, got {}", result.win_rate
    );
    assert_eq!(result.trades_executed, 4);
}

// ── Test 10: metrics max drawdown is computed correctly ───────────────────────

#[test]
fn test_metrics_max_drawdown() {
    use common::{Portfolio, PortfolioUpdate};
    use chrono::Utc;

    let mut collector = SimMetricsCollector::new();
    let now = Utc::now();

    let pnls = [100.0, 150.0, 80.0, 120.0, 60.0, 200.0]; // equity series

    for &pnl in &pnls {
        collector.on_portfolio(&PortfolioUpdate {
            portfolio: Portfolio { positions: vec![], pnl, exposure: 0.0 },
            timestamp: now,
        });
    }

    // Trace through the running peak: 100→150→80→120→60→200.
    // Peak never resets to 120 because the prior peak of 150 is higher.
    // Drawdown at equity=60 is (150-60)/150 = 0.60, which is the maximum.
    let result = collector.compute_result(0, 1_000.0);
    let max_expected = (150.0 - 60.0) / 150.0; // 0.60
    assert!(
        (result.max_drawdown - max_expected).abs() < 1e-9,
        "expected max_drawdown ≈ {max_expected:.4}, got {:.4}", result.max_drawdown
    );
}

// ── Test 11: HistoricalReplayer is empty for non-existent directory ───────────

#[test]
fn test_historical_replayer_missing_dir_is_empty() {
    let replayer = HistoricalReplayer::from_dir(Path::new("/nonexistent/path/xyz"));
    assert!(replayer.is_empty());
    assert_eq!(replayer.remaining(), 0);
}

// ── Test 12: generate_market_tick produces correct market count ───────────────

#[test]
fn test_generate_market_tick_market_count() {
    let bus = EventBus::new();
    let cfg = SimulationConfig {
        markets: three_market_specs(),
        ..fast_config()
    };
    let mut engine = SimulationEngine::new(cfg, bus);
    let tick = engine.generate_market_tick();
    assert_eq!(
        tick.market_updates.len(),
        3,
        "tick should contain one update per configured market"
    );
}

// ── Test 13: parameter sweep returns one SweepResult per point ───────────────

#[test]
fn test_parameter_sweep_result_count() {
    let bus = EventBus::new();
    let cfg = fast_config();
    let mut engine = SimulationEngine::new(cfg, bus);

    let points = vec![
        ParameterSweepPoint {
            label:     "low".to_string(),
            mc_config: MonteCarloConfig { volatility: 0.01, runs: 3, seed: Some(1), ..MonteCarloConfig::default() },
        },
        ParameterSweepPoint {
            label:     "mid".to_string(),
            mc_config: MonteCarloConfig { volatility: 0.03, runs: 3, seed: Some(2), ..MonteCarloConfig::default() },
        },
        ParameterSweepPoint {
            label:     "high".to_string(),
            mc_config: MonteCarloConfig { volatility: 0.07, runs: 3, seed: Some(3), ..MonteCarloConfig::default() },
        },
    ];

    let results = engine.run_parameter_sweep(points);
    assert_eq!(results.len(), 3, "expected one SweepResult per sweep point");
    for sr in &results {
        assert_eq!(sr.results.len(), 3, "each sweep point ran 3 MC runs");
    }
}

// ── Test 14: engine runs and cancels cleanly in LiveObserver mode ─────────────

#[tokio::test]
async fn test_engine_live_observer_cancels_cleanly() {
    use tokio_util::sync::CancellationToken;

    let bus    = EventBus::new();
    let cfg    = SimulationConfig {
        mode: SimulationMode::LiveObserver,
        ..SimulationConfig::default()
    };
    let cancel = CancellationToken::new();
    let engine = SimulationEngine::new(cfg, bus);

    let child  = cancel.child_token();
    let handle = tokio::spawn(engine.run(child));

    // Let the engine start, then cancel immediately.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    cancel.cancel();

    // Should finish without panicking or hanging.
    tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .expect("engine task timed out")
        .expect("engine task panicked");
}

// ── Test 15: historical replay publishes Event::Market to the bus ─────────────

#[tokio::test]
async fn test_historical_replay_publishes_market_events() {
    use common::Event;
    use std::io::Write;
    use tempfile::tempdir;

    // Write a small NDJSON file with two tick records.
    let dir  = tempdir().expect("tempdir");
    let path = dir.path().join("test_session.ndjson");
    let mut f = std::fs::File::create(&path).unwrap();

    let line1 = r#"{"timestamp":1000,"updates":[{"market":{"id":"SIM-A","probability":0.55,"liquidity":1000.0,"last_update":"2026-01-01T00:00:00Z"}}]}"#;
    let line2 = r#"{"timestamp":2000,"updates":[{"market":{"id":"SIM-A","probability":0.58,"liquidity":1000.0,"last_update":"2026-01-01T00:01:00Z"}}]}"#;
    writeln!(f, "{line1}").unwrap();
    writeln!(f, "{line2}").unwrap();
    drop(f);

    let bus = EventBus::new();
    let mut rx = bus.subscribe();

    let mut cfg = SimulationConfig::default();
    cfg.history_dir    = dir.path().to_string_lossy().to_string();
    cfg.tick_interval_ms = 0; // no sleep

    let mut engine = SimulationEngine::new(cfg, bus);
    let published  = engine.run_historical_replay().await;

    assert_eq!(published, 2, "expected 2 ticks published");

    // Both ticks should produce a Market event.
    let ev1 = rx.recv().await.unwrap();
    let ev2 = rx.recv().await.unwrap();
    assert!(matches!(ev1.as_ref(), Event::Market(_)));
    assert!(matches!(ev2.as_ref(), Event::Market(_)));
}
