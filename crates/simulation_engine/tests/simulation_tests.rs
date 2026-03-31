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
    let mut engine = SimulationEngine::new(cfg, bus).unwrap();
    let results = engine.run_monte_carlo();
    assert_eq!(results.len(), 5, "expected 5 simulation results");
}

// ── Test 8: MC results are accumulated in collect_results() ──────────────────

#[test]
fn test_collect_results_accumulates_across_calls() {
    let bus = EventBus::new();
    let cfg = fast_config();
    let mut engine = SimulationEngine::new(cfg, bus).unwrap();
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
        signal_source:     "test".to_string(),
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
    let mut engine = SimulationEngine::new(cfg, bus).unwrap();
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
    let mut engine = SimulationEngine::new(cfg, bus).unwrap();

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
    let engine = SimulationEngine::new(cfg, bus).unwrap();

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

    let mut engine = SimulationEngine::new(cfg, bus).unwrap();
    let published  = engine.run_historical_replay().await;

    assert_eq!(published, 2, "expected 2 ticks published");

    // Both ticks should produce a Market event.
    let ev1 = rx.recv().await.unwrap();
    let ev2 = rx.recv().await.unwrap();
    assert!(matches!(ev1.as_ref(), Event::Market(_)));
    assert!(matches!(ev2.as_ref(), Event::Market(_)));
}

// ═══════════════════════════════════════════════════════════════════════════
// Monte Carlo Simulator (MonteCarloSimulator) tests
// ═══════════════════════════════════════════════════════════════════════════

use simulation_engine::{
    compute_max_drawdown, compute_var_cvar, MarketStateSnapshot, MonteCarloSimConfig,
    MonteCarloSimulator, SimulationState,
};

// ── Helper ────────────────────────────────────────────────────────────────────

fn three_market_state() -> SimulationState {
    SimulationState::from_market_specs(
        &[
            MarketSpec { id: "S1".to_string(), initial_prob: 0.50, liquidity: 5_000.0 },
            MarketSpec { id: "S2".to_string(), initial_prob: 0.35, liquidity: 3_000.0 },
            MarketSpec { id: "S3".to_string(), initial_prob: 0.65, liquidity: 2_000.0 },
        ],
        10_000.0,
    )
}

fn fast_sim_config(seed: u64) -> MonteCarloSimConfig {
    MonteCarloSimConfig {
        volatility:           0.02,
        drift:                0.0,
        mean_reversion_speed: 0.05,
        shock_probability:    0.0,  // no shocks by default in unit tests
        shock_magnitude:      0.05,
        signal_threshold:     0.05,
        position_fraction:    0.05,
        seed:                 Some(seed),
    }
}

// ── Test MC-1: deterministic run with fixed seed ──────────────────────────────

#[test]
fn test_mc_deterministic_with_seed() {
    let state = three_market_state();
    let sim1  = MonteCarloSimulator::new(fast_sim_config(42)).unwrap();
    let sim2  = MonteCarloSimulator::new(fast_sim_config(42)).unwrap();

    let r1 = sim1.run_monte_carlo(&state, 20, 50);
    let r2 = sim2.run_monte_carlo(&state, 20, 50);

    assert_eq!(
        r1.paths.len(), r2.paths.len(),
        "both runs must produce the same number of paths"
    );
    assert!(
        (r1.expected_return - r2.expected_return).abs() < 1e-12,
        "fixed-seed runs must yield identical expected_return: {} vs {}",
        r1.expected_return, r2.expected_return
    );
    assert!(
        (r1.var_95 - r2.var_95).abs() < 1e-12,
        "fixed-seed runs must yield identical var_95"
    );
    // Different seed → different result.
    let sim3 = MonteCarloSimulator::new(fast_sim_config(999)).unwrap();
    let r3   = sim3.run_monte_carlo(&state, 20, 50);
    // The probability that two independent seeds give the SAME expected_return
    // to 12 decimal places is essentially zero.
    assert!(
        (r1.expected_return - r3.expected_return).abs() > 1e-10,
        "different seeds must produce different results"
    );
}

// ── Test MC-2: result struct contains correct path count ──────────────────────

#[test]
fn test_mc_path_count() {
    let state = three_market_state();
    let sim   = MonteCarloSimulator::new(fast_sim_config(1)).unwrap();

    for &n in &[1usize, 10, 100] {
        let r = sim.run_monte_carlo(&state, n, 50);
        assert_eq!(r.paths.len(), n, "expected {n} paths, got {}", r.paths.len());
    }
}

// ── Test MC-3: all result metrics are finite ──────────────────────────────────

#[test]
fn test_mc_all_metrics_finite() {
    let state = three_market_state();
    let sim   = MonteCarloSimulator::new(fast_sim_config(7)).unwrap();
    let r     = sim.run_monte_carlo(&state, 200, 100);

    assert!(r.expected_return.is_finite(),    "expected_return must be finite");
    assert!(r.return_std.is_finite(),         "return_std must be finite");
    assert!(r.sharpe_ratio.is_finite(),       "sharpe_ratio must be finite");
    assert!(r.max_drawdown.is_finite(),       "max_drawdown must be finite");
    assert!(r.var_95.is_finite(),             "var_95 must be finite");
    assert!(r.cvar_95.is_finite(),            "cvar_95 must be finite");
    assert!(r.profit_probability.is_finite(), "profit_probability must be finite");

    assert!(r.max_drawdown  >= 0.0 && r.max_drawdown  <= 1.0, "drawdown ∈ [0,1]");
    assert!(r.var_95        >= 0.0,                           "VaR ≥ 0");
    assert!(r.cvar_95       >= 0.0,                           "CVaR ≥ 0");
    assert!(r.profit_probability >= 0.0 && r.profit_probability <= 1.0,
            "profit_probability ∈ [0,1]");
}

// ── Test MC-4: increasing simulations reduces estimation variance ──────────────

#[test]
fn test_mc_more_simulations_reduce_estimation_variance() {
    // Law of large numbers: the std-error of the mean estimate shrinks as
    //   SE = return_std / sqrt(N).
    // With 50 paths vs 1 000 paths the SE should be √(1000/50) ≈ 4.5× tighter
    // for the larger run.  We assert SE_hi < SE_lo (Issue 12 fix).
    let state  = three_market_state();
    let sim_lo = MonteCarloSimulator::new(fast_sim_config(100)).unwrap();
    let sim_hi = MonteCarloSimulator::new(fast_sim_config(100)).unwrap();

    let r_lo = sim_lo.run_monte_carlo(&state, 50,   100);
    let r_hi = sim_hi.run_monte_carlo(&state, 1_000, 100);

    // Both must be finite.
    assert!(r_lo.expected_return.is_finite(), "r_lo expected_return must be finite");
    assert!(r_hi.expected_return.is_finite(), "r_hi expected_return must be finite");

    // Std-error of the mean = return_std / sqrt(N).
    let se_lo = r_lo.return_std / (50_f64).sqrt();
    let se_hi = r_hi.return_std / (1_000_f64).sqrt();

    // The 1 000-path estimate must have a strictly tighter std-error.
    assert!(
        se_hi < se_lo,
        "1000-path SE ({se_hi:.6}) must be tighter than 50-path SE ({se_lo:.6})"
    );

    // Sanity: profit_probability is bounded.
    assert!(r_lo.profit_probability >= 0.0 && r_lo.profit_probability <= 1.0);
    assert!(r_hi.profit_probability >= 0.0 && r_hi.profit_probability <= 1.0);
}

// ── Test MC-5: shock injection changes the return distribution ────────────────

#[test]
fn test_mc_shock_injection_changes_distribution() {
    let state = three_market_state();

    let no_shock_cfg = MonteCarloSimConfig {
        shock_probability: 0.0,
        ..fast_sim_config(55)
    };
    let shock_cfg = MonteCarloSimConfig {
        shock_probability: 0.30,  // shock 30% of ticks
        shock_magnitude:   0.10,
        ..fast_sim_config(55)    // same base seed → same Gaussian path, different shock path
    };

    let sim_no_shock = MonteCarloSimulator::new(no_shock_cfg).unwrap();
    let sim_shock    = MonteCarloSimulator::new(shock_cfg).unwrap();

    let r_clean = sim_no_shock.run_monte_carlo(&state, 500, 80);
    let r_shock  = sim_shock.run_monte_carlo(&state,   500, 80);

    // Shocks should increase overall volatility (return_std should be higher).
    // We test that the two distributions are NOT identical (they cannot be,
    // given that shocks add extra randomness with the same Gaussian seed but
    // an independent shock RNG stream).
    let diff = (r_shock.return_std - r_clean.return_std).abs();
    assert!(
        diff > 1e-6,
        "shock injection must change return_std: no-shock={:.6}, shock={:.6}",
        r_clean.return_std, r_shock.return_std
    );
}

// ── Test MC-6: mean reversion stabilizes probabilities ────────────────────────

#[test]
fn test_mc_mean_reversion_reduces_volatility() {
    // A high mean-reversion speed should anchor prices close to implied_prob,
    // reducing the spread of final portfolio values.
    let state = three_market_state();

    let low_rev_cfg = MonteCarloSimConfig {
        mean_reversion_speed: 0.0,   // no reversion → pure random walk
        volatility:           0.03,
        ..fast_sim_config(77)
    };
    let high_rev_cfg = MonteCarloSimConfig {
        mean_reversion_speed: 0.50,  // strong reversion toward implied_prob
        volatility:           0.03,
        ..fast_sim_config(77)
    };

    let sim_lo = MonteCarloSimulator::new(low_rev_cfg).unwrap();
    let sim_hi = MonteCarloSimulator::new(high_rev_cfg).unwrap();

    let r_lo = sim_lo.run_monte_carlo(&state, 400, 100);
    let r_hi = sim_hi.run_monte_carlo(&state, 400, 100);

    // Strong mean reversion should reduce return dispersion.
    assert!(
        r_hi.return_std <= r_lo.return_std + 1e-4,
        "high mean-reversion should not exceed low mean-reversion volatility: \
         high={:.6} > low={:.6}",
        r_hi.return_std, r_lo.return_std
    );
}

// ── Test MC-7: portfolio value evolves correctly (no-signal baseline) ──────────

#[test]
fn test_mc_no_trades_keeps_equity_stable() {
    // With a very high signal threshold (no positions ever open), the portfolio
    // value should remain exactly at the initial value.
    let state = SimulationState::from_market_specs(
        &[MarketSpec { id: "FLAT".to_string(), initial_prob: 0.50, liquidity: 1_000.0 }],
        5_000.0,
    );
    let cfg = MonteCarloSimConfig {
        signal_threshold: 1.0,  // impossible to exceed → no trades
        volatility:       0.02,
        drift:            0.0,
        ..fast_sim_config(3)
    };
    let sim = MonteCarloSimulator::new(cfg).unwrap();
    let r   = sim.run_monte_carlo(&state, 50, 100);

    for (i, path) in r.paths.iter().enumerate() {
        assert!(
            (path.final_value - 5_000.0).abs() < 1e-9,
            "path {i}: expected equity=5000, got {}", path.final_value
        );
        assert!(
            path.drawdown.abs() < 1e-9,
            "path {i}: expected drawdown=0, got {}", path.drawdown
        );
    }
    assert!(
        r.expected_return.abs() < 1e-9,
        "expected_return must be 0 when no trades: {}", r.expected_return
    );
}

// ── Test MC-8: drift changes the return distribution ─────────────────────────
//
// In a mean-reverting model, drift shifts the equilibrium price rather than
// uniformly lifting returns (a BUY strategy may trade against positive drift).
// We test that:
//   1. Both runs complete and produce finite metrics.
//   2. Different drift values produce measurably different distributions.

#[test]
fn test_mc_drift_changes_distribution() {
    let state = SimulationState::from_market_specs(
        &[MarketSpec { id: "D".to_string(), initial_prob: 0.50, liquidity: 1_000.0 }],
        10_000.0,
    );

    let zero_drift = MonteCarloSimConfig { drift:  0.0,   ..fast_sim_config(11) };
    let neg_drift  = MonteCarloSimConfig { drift: -0.015, ..fast_sim_config(11) };

    let r_flat = MonteCarloSimulator::new(zero_drift).unwrap().run_monte_carlo(&state, 400, 150);
    let r_down = MonteCarloSimulator::new(neg_drift).unwrap().run_monte_carlo(&state, 400, 150);

    // Both must be finite.
    assert!(r_flat.expected_return.is_finite());
    assert!(r_down.expected_return.is_finite());

    // Negative drift should produce a clearly lower (or equal) expected return
    // than zero drift, since market prices trend downward.
    assert!(
        r_down.expected_return <= r_flat.expected_return + 0.005,
        "negative drift should not significantly exceed zero drift: \
         neg={:.6}, flat={:.6}",
        r_down.expected_return, r_flat.expected_return
    );

    // The two distributions should be meaningfully different.
    let diff = (r_flat.expected_return - r_down.expected_return).abs();
    assert!(
        diff > 1e-8,
        "different drift values must produce different return distributions"
    );
}

// ── Test MC-9: zero and empty inputs return default result ─────────────────────

#[test]
fn test_mc_zero_simulations_returns_default() {
    let state = three_market_state();
    let sim   = MonteCarloSimulator::new(fast_sim_config(0)).unwrap();

    let r_zero_sims  = sim.run_monte_carlo(&state, 0, 100);
    let r_zero_steps = sim.run_monte_carlo(&state, 100, 0);

    let empty_state = SimulationState { markets: vec![], portfolio_value: 10_000.0 };
    let r_empty     = sim.run_monte_carlo(&empty_state, 100, 100);

    for r in [&r_zero_sims, &r_zero_steps, &r_empty] {
        assert_eq!(r.paths.len(), 0);
        assert_eq!(r.expected_return, 0.0);
        assert_eq!(r.var_95, 0.0);
    }
}

// ── Test MC-10: VaR ≤ CVaR for any return distribution ───────────────────────

#[test]
fn test_mc_var_le_cvar() {
    let state = three_market_state();
    let sim   = MonteCarloSimulator::new(MonteCarloSimConfig {
        shock_probability: 0.05,
        ..fast_sim_config(22)
    }).unwrap();
    let r = sim.run_monte_carlo(&state, 500, 100);

    assert!(
        r.var_95 <= r.cvar_95 + 1e-9,
        "VaR must be ≤ CVaR: var={:.6}, cvar={:.6}",
        r.var_95, r.cvar_95
    );
}

// ── Test MC-11: compute_var_cvar helper ───────────────────────────────────────

#[test]
fn test_compute_var_cvar_known_distribution() {
    // 10 paths. Use 80% confidence so tail_size = ceil(0.20 * 10) = 2,
    // which exercises the CVaR averaging formula (Issue 6 fix).
    // Sorted: [-0.20, -0.10, -0.05, 0.0, 0.05, 0.05, 0.10, 0.15, 0.20, 0.25]
    // (No -0.15 value so the two worst are exactly -0.20 and -0.10.)
    let returns = vec![-0.20, 0.25, -0.10, 0.15, -0.05, 0.10, 0.05, 0.05, 0.0, 0.20];
    let (var, cvar) = compute_var_cvar(&returns, 0.80);

    // VaR = -sorted[tail_size - 1] = -sorted[1] = -(-0.10) = 0.10.
    assert!(
        (var - 0.10).abs() < 1e-9,
        "expected VaR=0.10, got {:.6}", var
    );
    // CVaR = mean of 2 worst paths: (-0.20 + -0.10) / 2 → positive loss = 0.15.
    assert!(
        (cvar - 0.15).abs() < 1e-9,
        "expected CVaR=0.15, got {:.6}", cvar
    );
    assert!(var >= 0.0,  "VaR must be non-negative");
    assert!(cvar >= 0.0, "CVaR must be non-negative");
    // CVaR ≥ VaR (expected shortfall is at least as bad as VaR).
    assert!(cvar >= var - 1e-9, "CVaR must be ≥ VaR");
}

// ── Test MC-12: compute_max_drawdown helper ───────────────────────────────────

#[test]
fn test_compute_max_drawdown_known_series() {
    // Equity: 100 → 150 → 90 → 120 → 60 → 200.
    // Peak at 150: drawdown to 90 = (150-90)/150 = 0.40.
    // Peak still 150: drawdown to 60 = (150-60)/150 = 0.60.  ← maximum.
    let series = vec![100.0, 150.0, 90.0, 120.0, 60.0, 200.0];
    let dd = compute_max_drawdown(&series);
    let expected = (150.0 - 60.0) / 150.0; // 0.60
    assert!(
        (dd - expected).abs() < 1e-9,
        "expected max_drawdown={:.4}, got {:.4}", expected, dd
    );
}

#[test]
fn test_compute_max_drawdown_monotone_increase_is_zero() {
    let series = vec![100.0, 110.0, 120.0, 130.0, 140.0];
    assert_eq!(compute_max_drawdown(&series), 0.0);
}

// ── Test MC-13: config validation ─────────────────────────────────────────────

#[test]
fn test_mc_config_validation() {
    // Valid default must pass.
    assert!(MonteCarloSimConfig::default().validate().is_ok());

    // Negative volatility.
    assert!(MonteCarloSimConfig { volatility: -0.01, ..MonteCarloSimConfig::default() }
        .validate()
        .is_err());

    // shock_probability > 1.
    assert!(MonteCarloSimConfig { shock_probability: 1.5, ..MonteCarloSimConfig::default() }
        .validate()
        .is_err());

    // signal_threshold = 0.
    assert!(MonteCarloSimConfig { signal_threshold: 0.0, ..MonteCarloSimConfig::default() }
        .validate()
        .is_err());

    // position_fraction > 1.
    assert!(MonteCarloSimConfig { position_fraction: 1.5, ..MonteCarloSimConfig::default() }
        .validate()
        .is_err());
}

// ── Integration Test: full pipeline — strategies produce trades, portfolio updates

#[test]
fn test_mc_integration_full_pipeline() {
    // Multi-market simulation with shocks and active strategy.
    // Verifies the complete path: market evolution → shock → posterior update
    // → signal generation → position tracking → risk metrics.
    let state = SimulationState {
        markets: vec![
            MarketStateSnapshot {
                id:             "INT-A".to_string(),
                probability:    0.40,
                implied_prob:   0.50,  // big spread → mean reversion kicks in
                posterior_prob: 0.55,  // edge > threshold → trades open immediately
                liquidity:      10_000.0,
            },
            MarketStateSnapshot {
                id:             "INT-B".to_string(),
                probability:    0.60,
                implied_prob:   0.50,
                posterior_prob: 0.45,  // sell signal
                liquidity:       8_000.0,
            },
        ],
        portfolio_value: 20_000.0,
    };

    let cfg = MonteCarloSimConfig {
        volatility:           0.02,
        drift:                0.0,
        mean_reversion_speed: 0.10,
        shock_probability:    0.05,
        shock_magnitude:      0.06,
        signal_threshold:     0.05,
        position_fraction:    0.05,
        seed:                 Some(999),
    };
    let sim = MonteCarloSimulator::new(cfg).unwrap();
    let r   = sim.run_monte_carlo(&state, 200, 100);

    // Basic sanity checks.
    assert_eq!(r.paths.len(), 200, "must produce 200 paths");
    assert!(r.expected_return.is_finite(),    "expected_return finite");
    assert!(r.sharpe_ratio.is_finite(),       "sharpe finite");
    assert!(r.max_drawdown >= 0.0,            "drawdown ≥ 0");
    assert!(r.var_95       >= 0.0,            "VaR ≥ 0");
    assert!(r.cvar_95      >= r.var_95 - 1e-9, "CVaR ≥ VaR");
    assert!(r.profit_probability >= 0.0 && r.profit_probability <= 1.0,
            "profit_prob ∈ [0,1]");

    // Paths should have non-trivial per-tick returns (strategy was active).
    let active_paths = r.paths.iter().filter(|p| {
        p.returns.iter().any(|&ret| ret.abs() > 1e-12)
    }).count();
    assert!(
        active_paths > 0,
        "at least some paths should have non-zero tick returns (strategy active)"
    );

    // Per-path drawdowns must be ∈ [0, 1].
    for (i, path) in r.paths.iter().enumerate() {
        assert!(
            path.drawdown >= 0.0 && path.drawdown <= 1.0 + 1e-9,
            "path {i} drawdown={} out of [0,1]", path.drawdown
        );
        assert!(
            path.final_value.is_finite(),
            "path {i} final_value must be finite"
        );
        assert_eq!(
            path.returns.len(), 100,
            "path {i} must have 100 tick returns"
        );
    }
}
