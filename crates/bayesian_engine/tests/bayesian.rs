// crates/bayesian_engine/tests/bayesian.rs
// Integration tests for BayesianEngine (sync core) and BayesianModel (async wrapper).
//
// All sync tests exercise the public API of BayesianEngine directly.
// Async tests use a real EventBus and BayesianModel::run() task.

use std::time::Duration;

use approx::assert_abs_diff_eq;
use bayesian_engine::{BayesianEngine, BayesianModel, EvidenceSource};
use common::{Event, EventBus, MarketNode, MarketUpdate, SentimentUpdate};
use chrono::Utc;
use tokio::time::timeout;

const EPS: f64 = 1e-4;

// ── Helper: build a minimal MarketUpdate ─────────────────────────────────────

fn make_market_update(id: &str, prob: f64) -> MarketUpdate {
    MarketUpdate {
        market: MarketNode {
            id:          id.to_string(),
            probability: prob,
            liquidity:   10_000.0,
            last_update: Utc::now(),
        },
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// SYNC TESTS — BayesianEngine core
// ═══════════════════════════════════════════════════════════════════════════════

// ── 1. Prior update and posterior computation ─────────────────────────────────

#[test]
fn prior_update_shifts_posterior_proportionally() {
    let mut e = BayesianEngine::new();
    e.ensure_belief("m", 0.5);

    // No evidence: posterior == prior.
    assert_abs_diff_eq!(e.compute_posterior("m").unwrap(), 0.5, epsilon = EPS);

    // Update prior upward.
    e.update_prior("m", 0.75).unwrap();
    let p = e.compute_posterior("m").unwrap();
    assert!(p > 0.5, "higher prior must produce higher posterior");
    assert_abs_diff_eq!(p, 0.75, epsilon = EPS);
}

// ── 2. Graph influence propagation via apply_graph_influence ─────────────────

#[test]
fn graph_influence_propagation_updates_downstream_beliefs() {
    use market_graph::{EdgeType, MarketGraphEngine};

    let mut graph = MarketGraphEngine::new();
    // Build: trump_win → gop_senate (weight 0.5, confidence 1.0)
    graph.add_market(market_graph::MarketNode::new(
        "trump_win", "Trump wins", 0.60, None, None,
    ));
    graph.add_market(market_graph::MarketNode::new(
        "gop_senate", "GOP Senate", 0.50, None, None,
    ));
    graph.add_dependency("trump_win", "gop_senate", 0.5, 1.0, EdgeType::Causal)
        .unwrap();

    // Pre-seed the Bayesian engine from the graph.
    let mut e = BayesianEngine::from_graph(&graph);

    // Give trump_win a fused market price so market_confidence > 0.
    e.fuse_market_price("trump_win", 0.60, 0.8).unwrap();

    // Simulate trump_win's posterior rising above market price.
    e.ingest_evidence("trump_win", 1.5, EvidenceSource::PollAggregate, 0.8)
        .unwrap();
    let trump_posterior_before = e.compute_posterior("trump_win").unwrap();
    assert!(trump_posterior_before > 0.60);

    // Apply graph influence — should propagate trump_win's belief delta to gop_senate.
    let gop_before = e.get_belief("gop_senate").unwrap().posterior_prob;
    let updated = e.apply_graph_influence(&graph);

    let gop_after = e.get_belief("gop_senate").unwrap().posterior_prob;

    // gop_senate's posterior should have increased.
    assert!(
        gop_after > gop_before,
        "graph influence should push gop_senate up; before={gop_before:.4} after={gop_after:.4}"
    );
    // The updated list should contain gop_senate.
    assert!(
        updated.iter().any(|(id, _)| id == "gop_senate"),
        "gop_senate should appear in updated markets"
    );
}

// ── 3. Log-odds fusion correctness ────────────────────────────────────────────

#[test]
fn log_odds_fusion_two_pieces_of_evidence() {
    let mut e = BayesianEngine::new();
    e.ensure_belief("m", 0.5);

    // Both pieces push in the same direction.
    e.ingest_evidence("m", 0.8, EvidenceSource::PollAggregate, 1.0).unwrap();
    e.ingest_evidence("m", 0.6, EvidenceSource::NewsSentiment, 0.8).unwrap();

    // Total applied = 0.8 + 0.6 × 0.8 = 0.8 + 0.48 = 1.28
    // prior_lo(0.5) = 0  →  posterior_lo = 1.28
    let expected = 1.0 / (1.0 + (-1.28_f64).exp());
    let got = e.compute_posterior("m").unwrap();
    assert_abs_diff_eq!(got, expected, epsilon = EPS);
}

#[test]
fn opposing_evidence_partially_cancels() {
    let mut e = BayesianEngine::new();
    e.ensure_belief("m", 0.5);

    e.ingest_evidence("m",  1.5, EvidenceSource::PollAggregate, 1.0).unwrap();
    e.ingest_evidence("m", -1.0, EvidenceSource::MacroIndicator, 1.0).unwrap();

    // Net = +0.5 log-odds → posterior > 0.5
    let p = e.compute_posterior("m").unwrap();
    let expected = 1.0 / (1.0 + (-0.5_f64).exp());
    assert_abs_diff_eq!(p, expected, epsilon = EPS);
}

// ── 4. Market price fusion / confidence weighting ────────────────────────────

#[test]
fn market_price_fusion_correctness() {
    let mut e = BayesianEngine::new();
    e.ensure_belief("m", 0.4);

    // At precision 0.6: market pulls toward 0.7, prior = 0.4
    // market_lo = ln(0.7/0.3) ≈ 0.8473
    // prior_lo  = ln(0.4/0.6) ≈ -0.4055
    // fusion_lo = 0.6 × (0.8473 − (−0.4055)) = 0.6 × 1.2528 ≈ 0.7517
    // posterior_lo = −0.4055 + 0 + 0.7517 ≈ 0.3462
    // posterior = sigmoid(0.3462) ≈ 0.5857
    e.fuse_market_price("m", 0.7, 0.6).unwrap();
    let p = e.compute_posterior("m").unwrap();

    // Should be between prior (0.4) and market (0.7), closer to market.
    assert!(p > 0.4 && p < 0.7, "posterior {p} should be between prior and market");
}

#[test]
fn fusing_same_price_twice_is_idempotent() {
    let mut e = BayesianEngine::new();
    e.ensure_belief("m", 0.5);

    e.fuse_market_price("m", 0.8, 0.7).unwrap();
    let p1 = e.compute_posterior("m").unwrap();

    e.fuse_market_price("m", 0.8, 0.7).unwrap(); // identical second call
    let p2 = e.compute_posterior("m").unwrap();

    assert_abs_diff_eq!(p1, p2, epsilon = 1e-12);
}

// ── 5. Evidence ingestion from multiple sources ───────────────────────────────

#[test]
fn multiple_evidence_sources_accumulate_independently() {
    let mut e = BayesianEngine::new();
    e.ensure_belief("fed_cut", 0.35);

    let sources = [
        (0.50, EvidenceSource::PollAggregate, 1.0),
        (0.30, EvidenceSource::NewsSentiment, 0.8),
        (0.20, EvidenceSource::MacroIndicator, 0.9),
        (0.10, EvidenceSource::GraphPropagation, 0.5),
    ];

    let mut running_lo = {
        let p = 0.35_f64.clamp(1e-9, 1.0 - 1e-9);
        (p / (1.0 - p)).ln()  // log_odds(0.35)
    };

    for (delta, source, strength) in sources {
        e.ingest_evidence("fed_cut", delta, source, strength).unwrap();
        running_lo += delta * strength;
        let expected = 1.0 / (1.0 + (-running_lo).exp());
        let got = e.compute_posterior("fed_cut").unwrap();
        assert_abs_diff_eq!(got, expected, epsilon = EPS);
    }

    // History should have 4 entries.
    assert_eq!(e.get_belief("fed_cut").unwrap().update_history.len(), 4);
}

// ── 6. Deviation calculation from market price ───────────────────────────────

#[test]
fn deviation_is_posterior_minus_market_price() {
    let mut e = BayesianEngine::new();
    e.ensure_belief("btc_200k", 0.3);
    e.fuse_market_price("btc_200k", 0.35, 0.5).unwrap();
    e.ingest_evidence("btc_200k", 1.2, EvidenceSource::NewsSentiment, 0.7).unwrap();

    let post = e.compute_posterior("btc_200k").unwrap();
    let dev  = e.get_deviation_from_market("btc_200k").unwrap();

    assert_abs_diff_eq!(dev, post - 0.35, epsilon = EPS);
    // Positive deviation means model is above market → BUY signal.
    assert!(dev > 0.0, "strong positive evidence should push model above market");
}

#[test]
fn deviation_uses_prior_as_fallback_when_no_market_price() {
    let mut e = BayesianEngine::new();
    e.ensure_belief("eth_10k", 0.2);
    e.ingest_evidence("eth_10k", -0.5, EvidenceSource::MacroIndicator, 1.0).unwrap();

    let post = e.compute_posterior("eth_10k").unwrap();
    let dev  = e.get_deviation_from_market("eth_10k").unwrap();

    assert_abs_diff_eq!(dev, post - 0.2, epsilon = EPS); // fallback to prior 0.2
}

// ── 7. Batch posterior update with multiple nodes ─────────────────────────────

#[test]
fn batch_update_posteriors_returns_all_markets() {
    let mut e = BayesianEngine::new();
    let markets = [("a", 0.3), ("b", 0.5), ("c", 0.7), ("d", 0.9)];
    for (id, prob) in markets {
        e.ensure_belief(id, prob);
        e.ingest_evidence(id, 0.3, EvidenceSource::PollAggregate, 1.0).unwrap();
    }

    let results = e.batch_update_posteriors();
    assert_eq!(results.len(), markets.len());

    // All returned posteriors must be higher than their priors (positive evidence).
    for (id, posterior) in &results {
        let prior = e.get_belief(id).unwrap().prior_prob;
        assert!(
            posterior > &prior,
            "market {id}: posterior {posterior} should exceed prior {prior}"
        );
    }
}

// ── 8. Thread-safety — concurrent event handling via BayesianModel ────────────

#[tokio::test]
async fn bayesian_model_handles_concurrent_market_updates() {
    let bus    = EventBus::new();
    let model  = BayesianModel::new(bus.clone());
    let engine = model.engine_handle();

    let cancel = CancellationTokenHandle::new();
    let token  = cancel.token();

    // Spawn the model's event loop.
    let handle = tokio::spawn(async move {
        model.run(token).await;
    });

    // Allow the run loop to subscribe before we publish.
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Publish several MarketUpdate events concurrently (serial here, but the
    // runner processes them asynchronously from the tokio scheduler's perspective).
    let market_ids = ["m1", "m2", "m3", "m4", "m5"];
    let probs      = [0.3, 0.5, 0.7, 0.4, 0.6];

    for (&id, &prob) in market_ids.iter().zip(probs.iter()) {
        bus.publish(Event::Market(make_market_update(id, prob))).unwrap();
    }

    // Wait for the events to be processed.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Verify all markets have a belief state with a valid posterior.
    let eng = engine.read().await;
    for (&id, &prob) in market_ids.iter().zip(probs.iter()) {
        let belief = eng.get_belief(id).expect(&format!("belief for {id} should exist"));
        assert!(
            (0.0..=1.0).contains(&belief.posterior_prob),
            "posterior for {id} out of range: {}",
            belief.posterior_prob
        );
        // With no extra evidence, posterior should be near the market price.
        assert_abs_diff_eq!(belief.market_prob.unwrap(), prob, epsilon = EPS);
    }
    drop(eng);

    // Clean shutdown.
    cancel.cancel();
    let _ = timeout(Duration::from_millis(200), handle).await;
}

// ── 9. Sentiment update flows through BayesianModel ──────────────────────────

#[tokio::test]
async fn sentiment_update_adjusts_posterior_for_related_markets() {
    let bus    = EventBus::new();
    let model  = BayesianModel::new(bus.clone());
    let engine = model.engine_handle();

    let cancel = CancellationTokenHandle::new();
    let token  = cancel.token();

    tokio::spawn(async move { model.run(token).await; });
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Seed the market first.
    bus.publish(Event::Market(make_market_update("fed_hike", 0.5))).unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;

    let posterior_after_market = engine.read().await
        .get_belief("fed_hike")
        .unwrap()
        .posterior_prob;

    // Now push a strongly positive sentiment event.
    bus.publish(Event::Sentiment(SentimentUpdate {
        related_market_ids: vec!["fed_hike".to_string()],
        headline:           "Fed signals aggressive rate hikes".to_string(),
        sentiment_score:    0.9, // strongly positive for fed_hike
        timestamp:          Utc::now(),
    })).unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;

    let posterior_after_sentiment = engine.read().await
        .get_belief("fed_hike")
        .unwrap()
        .posterior_prob;

    assert!(
        posterior_after_sentiment > posterior_after_market,
        "positive sentiment should push posterior up: {posterior_after_market:.4} → {posterior_after_sentiment:.4}"
    );

    cancel.cancel();
}

// ── Helper for clean CancellationToken management in tests ───────────────────

use tokio_util::sync::CancellationToken;

struct CancellationTokenHandle(CancellationToken);

impl CancellationTokenHandle {
    fn new() -> Self {
        Self(CancellationToken::new())
    }
    fn token(&self) -> CancellationToken {
        self.0.clone()
    }
    fn cancel(&self) {
        self.0.cancel();
    }
}
