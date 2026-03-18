// crates/strategy_research/tests/research_tests.rs
//
// Unit and integration tests for the strategy_research crate.
//
// Unit tests cover each subsystem in isolation:
//   - Config validation
//   - Hypothesis generation (count, uniqueness, non-empty conditions)
//   - Strategy compilation and evaluate() logic
//   - Backtest runner (trade generation, equity curve)
//   - Evaluator (Sharpe, drawdown, win rate)
//   - Registry (promotion criteria, insertion, capacity eviction)
//
// Integration tests verify the full pipeline end-to-end:
//   - Multiple hypotheses → backtest → selection of top performers
//   - Event::StrategyDiscovered emitted for promoted strategies

use chrono::Utc;
use common::{Event, EventBus};
use strategy_research::{
    GeneratedStrategy, Hypothesis, ResearchConfig, StrategyMetrics,
    StrategyRegistry,
};
use strategy_research::{
    backtest::run_backtest,
    compiler::StrategyCompiler,
    evaluator::{evaluate, max_drawdown, sharpe, stddev},
    hypothesis::HypothesisGenerator,
    templates::all_templates,
    types::{
        BacktestResult, BacktestTrade, ComparisonOp, DirectionRule, LogicalOp, MarketSnapshot,
        SignalCondition, SignalFeature, SizingRule,
    },
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

// ── Helpers ──────────────────────────────────────────────────────────────────

fn default_snapshot() -> MarketSnapshot {
    MarketSnapshot {
        market_id: "test_market".into(),
        market_prob: 0.50,
        posterior_edge: 0.10,
        graph_arb_edge: 0.05,
        temporal_z_score: 1.8,
        shock_magnitude: 0.30,
        volatility: 0.015,
        posterior_prob: 0.60,
        implied_graph_prob: 0.55,
    }
}

fn make_hypothesis(
    feature: SignalFeature,
    op: ComparisonOp,
    threshold: f64,
    direction_rule: DirectionRule,
) -> Hypothesis {
    Hypothesis {
        id: Uuid::new_v4(),
        conditions: vec![SignalCondition { feature, op, threshold }],
        logical_op: LogicalOp::And,
        direction_rule,
        sizing_rule: SizingRule::Fixed(0.02),
        template_name: "Test".into(),
        created_at: Utc::now(),
    }
}

fn make_strategy(
    feature: SignalFeature,
    op: ComparisonOp,
    threshold: f64,
    direction_rule: DirectionRule,
) -> GeneratedStrategy {
    StrategyCompiler::compile(make_hypothesis(feature, op, threshold, direction_rule))
}

fn passing_metrics(strategy_id: Uuid) -> StrategyMetrics {
    StrategyMetrics {
        strategy_id,
        sharpe_ratio: 2.5,
        max_drawdown: 0.05,
        win_rate: 0.60,
        trade_count: 150,
        total_return: 0.15,
        volatility: 0.01,
        avg_trade_pnl: 10.0,
    }
}

// ── Config tests ──────────────────────────────────────────────────────────────

#[test]
fn config_default_is_valid() {
    assert!(ResearchConfig::default().validate().is_ok());
}

#[test]
fn config_rejects_zero_batch_size() {
    let mut c = ResearchConfig::default();
    c.batch_size = 0;
    assert!(c.validate().is_err());
}

#[test]
fn config_rejects_too_few_backtest_ticks() {
    let mut c = ResearchConfig::default();
    c.backtest_ticks = 50;
    assert!(c.validate().is_err());
}

#[test]
fn config_rejects_one_market() {
    let mut c = ResearchConfig::default();
    c.backtest_markets = 1;
    assert!(c.validate().is_err());
}

#[test]
fn config_rejects_invalid_drawdown() {
    let mut c = ResearchConfig::default();
    c.max_drawdown = 1.5;
    assert!(c.validate().is_err());
}

// ── Hypothesis generation tests ───────────────────────────────────────────────

#[test]
fn hypothesis_generates_requested_count() {
    let mut gen = HypothesisGenerator::new(Some(42), all_templates());
    let hyps = gen.generate(100);
    assert_eq!(hyps.len(), 100);
}

#[test]
fn hypothesis_conditions_are_non_empty() {
    let mut gen = HypothesisGenerator::new(Some(1), all_templates());
    for h in gen.generate(50) {
        assert!(!h.conditions.is_empty(), "hypothesis must have at least one condition");
    }
}

#[test]
fn hypothesis_ids_are_unique() {
    let mut gen = HypothesisGenerator::new(None, all_templates());
    let hyps = gen.generate(200);
    let unique: std::collections::HashSet<_> = hyps.iter().map(|h| h.id).collect();
    assert_eq!(unique.len(), hyps.len());
}

#[test]
fn hypothesis_thresholds_within_valid_range() {
    let mut gen = HypothesisGenerator::new(Some(99), all_templates());
    for h in gen.generate(200) {
        for c in &h.conditions {
            assert!(
                c.threshold.is_finite() && c.threshold > 0.0,
                "threshold must be finite and positive: {}", c.threshold
            );
        }
    }
}

#[test]
fn hypothesis_mutate_changes_id() {
    let mut gen = HypothesisGenerator::new(Some(7), all_templates());
    let original = gen.generate(1).pop().unwrap();
    let mutated = gen.mutate(&original);
    assert_ne!(original.id, mutated.id, "mutated hypothesis must have a new id");
}

// ── Strategy compilation and evaluate() tests ─────────────────────────────────

#[test]
fn strategy_fires_when_condition_met() {
    // PosteriorEdge AbsGreaterThan 0.05 → snapshot has 0.10 → should fire
    let strategy = make_strategy(
        SignalFeature::PosteriorEdge,
        ComparisonOp::AbsGreaterThan,
        0.05,
        DirectionRule::FollowPosterior,
    );
    let snapshot = default_snapshot(); // posterior_edge = 0.10
    assert!(strategy.evaluate(&snapshot).is_some());
}

#[test]
fn strategy_does_not_fire_when_condition_unmet() {
    // Threshold much higher than snapshot value → should not fire
    let strategy = make_strategy(
        SignalFeature::PosteriorEdge,
        ComparisonOp::AbsGreaterThan,
        0.50, // snapshot has 0.10
        DirectionRule::FollowPosterior,
    );
    let snapshot = default_snapshot();
    assert!(strategy.evaluate(&snapshot).is_none());
}

#[test]
fn strategy_direction_follow_posterior_positive_edge_is_buy() {
    let strategy = make_strategy(
        SignalFeature::PosteriorEdge,
        ComparisonOp::AbsGreaterThan,
        0.01,
        DirectionRule::FollowPosterior,
    );
    let snapshot = default_snapshot(); // posterior_edge = +0.10
    let (dir, _) = strategy.evaluate(&snapshot).unwrap();
    assert_eq!(dir, common::TradeDirection::Buy);
}

#[test]
fn strategy_direction_contrarian_positive_edge_is_sell() {
    let strategy = make_strategy(
        SignalFeature::PosteriorEdge,
        ComparisonOp::AbsGreaterThan,
        0.01,
        DirectionRule::Contrarian,
    );
    let snapshot = default_snapshot(); // posterior_edge = +0.10
    let (dir, _) = strategy.evaluate(&snapshot).unwrap();
    assert_eq!(dir, common::TradeDirection::Sell);
}

#[test]
fn strategy_direction_follow_momentum_positive_zscore_is_buy() {
    let strategy = make_strategy(
        SignalFeature::TemporalMomentum,
        ComparisonOp::AbsGreaterThan,
        0.5,
        DirectionRule::FollowMomentum,
    );
    let mut snapshot = default_snapshot();
    snapshot.temporal_z_score = 2.0; // positive momentum
    let (dir, _) = strategy.evaluate(&snapshot).unwrap();
    assert_eq!(dir, common::TradeDirection::Buy);
}

#[test]
fn strategy_or_logic_fires_on_any_condition() {
    let hyp = Hypothesis {
        id: Uuid::new_v4(),
        conditions: vec![
            // First condition will fail (threshold too high)
            SignalCondition {
                feature: SignalFeature::PosteriorEdge,
                op: ComparisonOp::AbsGreaterThan,
                threshold: 0.50,
            },
            // Second condition will pass
            SignalCondition {
                feature: SignalFeature::TemporalMomentum,
                op: ComparisonOp::AbsGreaterThan,
                threshold: 1.0,
            },
        ],
        logical_op: LogicalOp::Or,
        direction_rule: DirectionRule::FollowMomentum,
        sizing_rule: SizingRule::Fixed(0.02),
        template_name: "OrTest".into(),
        created_at: Utc::now(),
    };
    let strategy = StrategyCompiler::compile(hyp);
    let snapshot = default_snapshot(); // temporal_z_score = 1.8 > 1.0
    assert!(strategy.evaluate(&snapshot).is_some());
}

#[test]
fn strategy_and_logic_requires_all_conditions() {
    let hyp = Hypothesis {
        id: Uuid::new_v4(),
        conditions: vec![
            // Passes
            SignalCondition {
                feature: SignalFeature::PosteriorEdge,
                op: ComparisonOp::AbsGreaterThan,
                threshold: 0.05,
            },
            // Fails (threshold 5.0 is unreachable in normal conditions)
            SignalCondition {
                feature: SignalFeature::TemporalMomentum,
                op: ComparisonOp::AbsGreaterThan,
                threshold: 5.0,
            },
        ],
        logical_op: LogicalOp::And,
        direction_rule: DirectionRule::FollowPosterior,
        sizing_rule: SizingRule::Fixed(0.02),
        template_name: "AndTest".into(),
        created_at: Utc::now(),
    };
    let strategy = StrategyCompiler::compile(hyp);
    let snapshot = default_snapshot();
    assert!(strategy.evaluate(&snapshot).is_none());
}

#[test]
fn strategy_kelly_sizing_is_bounded() {
    let hyp = Hypothesis {
        id: Uuid::new_v4(),
        conditions: vec![SignalCondition {
            feature: SignalFeature::PosteriorEdge,
            op: ComparisonOp::AbsGreaterThan,
            threshold: 0.01,
        }],
        logical_op: LogicalOp::And,
        direction_rule: DirectionRule::FollowPosterior,
        sizing_rule: SizingRule::Kelly,
        template_name: "KellyTest".into(),
        created_at: Utc::now(),
    };
    let strategy = StrategyCompiler::compile(hyp);
    let snapshot = default_snapshot();
    let (_, fraction) = strategy.evaluate(&snapshot).unwrap();
    assert!(fraction > 0.0 && fraction <= 0.05, "Kelly fraction must be in (0, 0.05]: {fraction}");
}

// ── Backtest runner tests ──────────────────────────────────────────────────────

fn aggressive_strategy() -> GeneratedStrategy {
    // Very low threshold → fires on almost every tick
    make_strategy(
        SignalFeature::PosteriorEdge,
        ComparisonOp::AbsGreaterThan,
        0.001,
        DirectionRule::FollowPosterior,
    )
}

fn fast_config() -> ResearchConfig {
    ResearchConfig {
        backtest_ticks: 500,
        backtest_markets: 2,
        hold_ticks: 10,
        initial_capital: 10_000.0,
        ..ResearchConfig::default()
    }
}

#[test]
fn backtest_produces_trades_for_aggressive_strategy() {
    let strategy = aggressive_strategy();
    let config = fast_config();
    let result = run_backtest(&strategy, &config, 42);
    assert!(!result.trades.is_empty(), "aggressive strategy must generate trades");
}

#[test]
fn backtest_equity_curve_has_correct_length() {
    let strategy = aggressive_strategy();
    let config = fast_config();
    let result = run_backtest(&strategy, &config, 42);
    assert_eq!(result.equity_curve.len(), config.backtest_ticks);
}

#[test]
fn backtest_equity_starts_at_initial_capital() {
    let strategy = aggressive_strategy();
    let config = fast_config();
    let result = run_backtest(&strategy, &config, 1);
    // First equity point is initial capital (no trades closed yet).
    assert!(
        (result.equity_curve[0] - config.initial_capital).abs() < 1.0,
        "equity must start near initial_capital"
    );
}

#[test]
fn backtest_inactive_strategy_produces_no_trades() {
    // Threshold so high it never fires.
    let strategy = make_strategy(
        SignalFeature::PosteriorEdge,
        ComparisonOp::AbsGreaterThan,
        999.0,
        DirectionRule::FollowPosterior,
    );
    let config = fast_config();
    let result = run_backtest(&strategy, &config, 7);
    assert!(result.trades.is_empty(), "unreachable threshold → no trades");
}

#[test]
fn backtest_is_deterministic_with_same_seed() {
    let strategy = aggressive_strategy();
    let config = fast_config();
    let r1 = run_backtest(&strategy, &config, 123);
    let r2 = run_backtest(&strategy, &config, 123);
    assert_eq!(r1.trades.len(), r2.trades.len());
    assert_eq!(r1.equity_curve.last(), r2.equity_curve.last());
}

#[test]
fn backtest_differs_with_different_seeds() {
    let strategy = aggressive_strategy();
    let config = fast_config();
    let r1 = run_backtest(&strategy, &config, 1);
    let r2 = run_backtest(&strategy, &config, 2);
    // With different seeds, final equity is almost certainly different.
    assert_ne!(r1.equity_curve.last(), r2.equity_curve.last());
}

// ── Evaluator tests ───────────────────────────────────────────────────────────

#[test]
fn evaluate_empty_result_returns_worst_case() {
    let result = BacktestResult {
        strategy_id: Uuid::new_v4(),
        trades: vec![],
        equity_curve: vec![],
        initial_capital: 10_000.0,
    };
    let m = evaluate(&result);
    assert_eq!(m.trade_count, 0);
    assert_eq!(m.win_rate, 0.0);
    assert!(m.sharpe_ratio.is_infinite() && m.sharpe_ratio < 0.0);
    assert_eq!(m.max_drawdown, 1.0);
}

#[test]
fn evaluate_all_winning_trades() {
    let id = Uuid::new_v4();
    let trades: Vec<BacktestTrade> = (0..10)
        .map(|_| BacktestTrade {
            market_id: "m".into(),
            direction: common::TradeDirection::Buy,
            position_fraction: 0.02,
            entry_prob: 0.40,
            exit_prob: 0.50,
            pnl_fraction: 0.10,
            pnl_abs: 20.0,
        })
        .collect();

    // Build an equity curve rising by 20 each of 10 ticks.
    let equity_curve: Vec<f64> = (0..=10).map(|i| 10_000.0 + i as f64 * 20.0).collect();

    let result = BacktestResult {
        strategy_id: id,
        trades,
        equity_curve,
        initial_capital: 10_000.0,
    };
    let m = evaluate(&result);
    assert_eq!(m.trade_count, 10);
    assert_eq!(m.win_rate, 1.0);
    assert!(m.total_return > 0.0);
    assert!(m.sharpe_ratio > 0.0);
}

#[test]
fn sharpe_negative_for_always_losing_returns() {
    let returns: Vec<f64> = vec![-0.01; 100];
    assert!(sharpe(&returns) < 0.0);
}

#[test]
fn sharpe_positive_for_always_winning_returns() {
    let returns: Vec<f64> = vec![0.01; 100];
    assert!(sharpe(&returns) > 0.0);
}

#[test]
fn sharpe_neginf_for_single_return() {
    assert!(sharpe(&[0.05]).is_infinite());
}

#[test]
fn stddev_of_constant_series_is_zero() {
    assert_eq!(stddev(&[1.0, 1.0, 1.0, 1.0]), 0.0);
}

#[test]
fn max_drawdown_monotone_rising_is_zero() {
    let curve = vec![100.0, 110.0, 120.0, 130.0];
    assert_eq!(max_drawdown(&curve), 0.0);
}

#[test]
fn max_drawdown_50pct_drop() {
    let curve = vec![100.0, 50.0, 60.0];
    let dd = max_drawdown(&curve);
    assert!((dd - 0.50).abs() < 1e-9, "expected 50% drawdown, got {dd}");
}

// ── Registry tests ────────────────────────────────────────────────────────────

fn test_registry() -> StrategyRegistry {
    StrategyRegistry::new(&ResearchConfig {
        min_sharpe: 1.5,
        max_drawdown: 0.10,
        min_trades: 100,
        registry_capacity: 3,
        ..ResearchConfig::default()
    })
}

#[test]
fn registry_accepts_strategy_meeting_criteria() {
    let mut reg = test_registry();
    let id = Uuid::new_v4();
    let h = make_hypothesis(
        SignalFeature::PosteriorEdge,
        ComparisonOp::AbsGreaterThan,
        0.05,
        DirectionRule::FollowPosterior,
    );
    let result = reg.try_insert(h, passing_metrics(id));
    assert!(result.is_some());
    assert_eq!(reg.len(), 1);
}

#[test]
fn registry_meets_criteria_passes_good_metrics() {
    let config = ResearchConfig::default();
    let id = Uuid::new_v4();
    assert!(StrategyRegistry::meets_promotion_criteria(&passing_metrics(id), &config));
}

#[test]
fn registry_meets_criteria_fails_low_sharpe() {
    let config = ResearchConfig::default(); // min_sharpe = 1.5
    let bad = StrategyMetrics {
        sharpe_ratio: 0.5,
        ..passing_metrics(Uuid::new_v4())
    };
    assert!(!StrategyRegistry::meets_promotion_criteria(&bad, &config));
}

#[test]
fn registry_meets_criteria_fails_high_drawdown() {
    let config = ResearchConfig::default(); // max_drawdown = 0.10
    let bad = StrategyMetrics {
        max_drawdown: 0.15,
        ..passing_metrics(Uuid::new_v4())
    };
    assert!(!StrategyRegistry::meets_promotion_criteria(&bad, &config));
}

#[test]
fn registry_meets_criteria_fails_too_few_trades() {
    let config = ResearchConfig::default(); // min_trades = 100
    let bad = StrategyMetrics {
        trade_count: 50,
        ..passing_metrics(Uuid::new_v4())
    };
    assert!(!StrategyRegistry::meets_promotion_criteria(&bad, &config));
}

#[test]
fn registry_rejects_duplicate_id() {
    let mut reg = test_registry();
    let h1 = make_hypothesis(
        SignalFeature::PosteriorEdge,
        ComparisonOp::AbsGreaterThan,
        0.05,
        DirectionRule::FollowPosterior,
    );
    let id = h1.id;
    let h2 = Hypothesis { id, ..h1.clone() }; // same UUID
    reg.try_insert(h1, passing_metrics(id));
    let result = reg.try_insert(h2, passing_metrics(id));
    assert!(result.is_none(), "duplicate id must be rejected");
    assert_eq!(reg.len(), 1);
}

#[test]
fn registry_evicts_worst_when_at_capacity() {
    let mut reg = test_registry(); // capacity = 3

    for i in 0..3u32 {
        let h = make_hypothesis(
            SignalFeature::PosteriorEdge,
            ComparisonOp::AbsGreaterThan,
            0.05,
            DirectionRule::FollowPosterior,
        );
        let id = h.id;
        let m = StrategyMetrics {
            sharpe_ratio: 1.5 + i as f64 * 0.1, // 1.5, 1.6, 1.7
            ..passing_metrics(id)
        };
        reg.try_insert(h, m);
    }
    assert_eq!(reg.len(), 3);

    // Insert a strategy with Sharpe = 3.0 → should evict the worst (1.5).
    let best_h = make_hypothesis(
        SignalFeature::PosteriorEdge,
        ComparisonOp::AbsGreaterThan,
        0.05,
        DirectionRule::FollowPosterior,
    );
    let best_id = best_h.id;
    let best_m = StrategyMetrics {
        sharpe_ratio: 3.0,
        ..passing_metrics(best_id)
    };
    let result = reg.try_insert(best_h, best_m);
    assert!(result.is_some(), "better strategy must be accepted");
    assert_eq!(reg.len(), 3, "capacity must be maintained");

    // Verify the best strategy is at the top.
    let top = reg.top_strategies(1);
    assert_eq!(top[0].metrics.sharpe_ratio, 3.0);
}

#[test]
fn registry_rejects_inferior_when_at_capacity() {
    let mut reg = test_registry(); // capacity = 3

    // Fill with high-Sharpe strategies.
    for _ in 0..3 {
        let h = make_hypothesis(
            SignalFeature::PosteriorEdge,
            ComparisonOp::AbsGreaterThan,
            0.05,
            DirectionRule::FollowPosterior,
        );
        let id = h.id;
        let m = StrategyMetrics { sharpe_ratio: 5.0, ..passing_metrics(id) };
        reg.try_insert(h, m);
    }

    // Attempt to insert a worse strategy.
    let h = make_hypothesis(
        SignalFeature::PosteriorEdge,
        ComparisonOp::AbsGreaterThan,
        0.05,
        DirectionRule::FollowPosterior,
    );
    let id = h.id;
    let m = StrategyMetrics { sharpe_ratio: 1.6, ..passing_metrics(id) };
    let result = reg.try_insert(h, m);
    assert!(result.is_none(), "inferior strategy must be rejected when at capacity");
    assert_eq!(reg.len(), 3);
}

// ── Integration tests ─────────────────────────────────────────────────────────

/// Run a small full pipeline (generate → compile → backtest → evaluate → promote)
/// and verify that at least some strategies are promoted with very loose thresholds.
#[test]
fn integration_pipeline_promotes_strategies_with_loose_thresholds() {
    let config = ResearchConfig {
        batch_size: 50,
        backtest_ticks: 300,
        backtest_markets: 2,
        hold_ticks: 10,
        min_sharpe: f64::NEG_INFINITY,
        max_drawdown: 1.0,
        min_trades: 0,
        registry_capacity: 50,
        seed: Some(42),
        max_parallel_backtests: 4,
        ..ResearchConfig::default()
    };

    let mut gen = HypothesisGenerator::new(config.seed, all_templates());
    let strategies: Vec<_> = gen
        .generate(config.batch_size)
        .into_iter()
        .map(StrategyCompiler::compile)
        .collect();

    let mut reg = StrategyRegistry::new(&config);
    let mut promoted = 0;

    for strategy in &strategies {
        let result = run_backtest(strategy, &config, strategy.id.as_u128() as u64);
        let metrics = evaluate(&result);

        if reg.meets_criteria(&metrics) {
            if reg.try_insert(strategy.hypothesis.clone(), metrics).is_some() {
                promoted += 1;
            }
        }
    }

    assert!(promoted > 0, "at least one strategy should be promoted with loose thresholds");
}

/// Run a research cycle and confirm top strategies are sorted by Sharpe.
#[test]
fn integration_registry_top_strategies_sorted_by_sharpe() {
    let config = ResearchConfig {
        batch_size: 30,
        backtest_ticks: 200,
        backtest_markets: 2,
        hold_ticks: 10,
        min_sharpe: f64::NEG_INFINITY,
        max_drawdown: 1.0,
        min_trades: 0,
        registry_capacity: 20,
        seed: Some(7),
        ..ResearchConfig::default()
    };

    let mut gen = HypothesisGenerator::new(config.seed, all_templates());
    let mut reg = StrategyRegistry::new(&config);

    for strategy in gen
        .generate(config.batch_size)
        .into_iter()
        .map(StrategyCompiler::compile)
    {
        let result = run_backtest(&strategy, &config, strategy.id.as_u128() as u64);
        let metrics = evaluate(&result);
        reg.try_insert(strategy.hypothesis, metrics);
    }

    let top = reg.top_strategies(10);
    for window in top.windows(2) {
        assert!(
            window[0].metrics.sharpe_ratio >= window[1].metrics.sharpe_ratio,
            "top strategies must be sorted descending by Sharpe"
        );
    }
}

/// Full async engine test: verify Event::StrategyDiscovered is emitted within a
/// reasonable timeout when promotion criteria are very loose.
#[tokio::test]
async fn integration_engine_emits_strategy_discovered() {
    use strategy_research::StrategyResearchEngine;
    use tokio::time::{timeout, Duration};

    let bus = EventBus::new();
    let mut rx = bus.subscribe();

    let config = ResearchConfig {
        batch_size: 30,
        research_interval_secs: 1, // first cycle fires immediately
        backtest_ticks: 200,
        backtest_markets: 2,
        hold_ticks: 5,
        min_sharpe: f64::NEG_INFINITY,
        max_drawdown: 1.0,
        min_trades: 0,
        registry_capacity: 10,
        seed: Some(42),
        max_parallel_backtests: 4,
        ..ResearchConfig::default()
    };

    let engine = StrategyResearchEngine::new(config, bus.clone());
    let cancel = CancellationToken::new();

    let handle = tokio::spawn(engine.run(cancel.clone()));

    // Wait up to 15 seconds for at least one StrategyDiscovered event.
    let result = timeout(Duration::from_secs(15), async {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if matches!(ev.as_ref(), Event::StrategyDiscovered(_)) {
                        return true;
                    }
                }
                Err(_) => return false,
            }
        }
    })
    .await;

    cancel.cancel();
    let _ = handle.await;

    assert!(
        matches!(result, Ok(true)),
        "expected Event::StrategyDiscovered within 15 seconds"
    );
}

/// Verify that StrategyDiscovered events contain valid, non-empty data.
#[tokio::test]
async fn integration_strategy_discovered_event_has_valid_data() {
    use common::events::StrategyDiscoveredEvent;
    use strategy_research::StrategyResearchEngine;
    use tokio::time::{timeout, Duration};

    let bus = EventBus::new();
    let mut rx = bus.subscribe();

    let config = ResearchConfig {
        batch_size: 20,
        research_interval_secs: 1,
        backtest_ticks: 150,
        backtest_markets: 2,
        hold_ticks: 5,
        min_sharpe: f64::NEG_INFINITY,
        max_drawdown: 1.0,
        min_trades: 0,
        registry_capacity: 5,
        seed: Some(13),
        max_parallel_backtests: 4,
        ..ResearchConfig::default()
    };

    let engine = StrategyResearchEngine::new(config, bus.clone());
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(engine.run(cancel.clone()));

    let found: Option<StrategyDiscoveredEvent> = timeout(Duration::from_secs(15), async {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if let Event::StrategyDiscovered(e) = ev.as_ref() {
                        return Some(e.clone());
                    }
                }
                Err(_) => return None,
            }
        }
    })
    .await
    .ok()
    .flatten();

    cancel.cancel();
    let _ = handle.await;

    let event = found.expect("expected a StrategyDiscovered event");
    assert!(!event.strategy_id.is_empty());
    assert!(!event.rule_definition_json.is_empty());
    assert!(event.rule_definition_json.starts_with('{'));
    assert!(event.win_rate >= 0.0 && event.win_rate <= 1.0);
    assert!(event.max_drawdown >= 0.0 && event.max_drawdown <= 1.0);
}

/// Example of a complete research cycle showing hypothesis ranking and promotion.
#[test]
fn example_research_cycle() {
    let config = ResearchConfig {
        batch_size: 40,
        backtest_ticks: 500,
        backtest_markets: 3,
        hold_ticks: 15,
        min_sharpe: f64::NEG_INFINITY,
        max_drawdown: 1.0,
        min_trades: 0,
        registry_capacity: 5,
        seed: Some(2024),
        ..ResearchConfig::default()
    };

    // 1. Generate hypotheses.
    let mut gen = HypothesisGenerator::new(config.seed, all_templates());
    let strategies: Vec<_> = gen
        .generate(config.batch_size)
        .into_iter()
        .map(StrategyCompiler::compile)
        .collect();

    // 2. Backtest and evaluate all.
    let mut all_metrics: Vec<(GeneratedStrategy, strategy_research::StrategyMetrics)> = strategies
        .into_iter()
        .map(|s| {
            let seed = s.id.as_u128() as u64;
            let result = run_backtest(&s, &config, seed);
            let metrics = evaluate(&result);
            (s, metrics)
        })
        .collect();

    // 3. Sort by Sharpe to find top performers.
    all_metrics.sort_by(|a, b| {
        b.1.sharpe_ratio.partial_cmp(&a.1.sharpe_ratio).unwrap_or(std::cmp::Ordering::Equal)
    });

    // 4. Promote top-5 into registry.
    let mut reg = StrategyRegistry::new(&config);
    let mut promoted_count = 0;
    for (strategy, metrics) in all_metrics.iter().take(5) {
        if reg.try_insert(strategy.hypothesis.clone(), metrics.clone()).is_some() {
            promoted_count += 1;
        }
    }

    assert!(promoted_count > 0);
    assert!(reg.len() <= 5);

    // 5. Verify registry ordering.
    let top = reg.top_strategies(5);
    assert!(!top.is_empty());
    // Best strategy's Sharpe must be >= any other in registry.
    let best_sharpe = top[0].metrics.sharpe_ratio;
    for s in &top {
        assert!(s.metrics.sharpe_ratio <= best_sharpe + 1e-9);
    }
}
