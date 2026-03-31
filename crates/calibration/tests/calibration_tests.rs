// crates/calibration/tests/calibration_tests.rs

use std::time::Duration;

use chrono::Utc;
use common::{Event, EventBus, MarketNode, MarketUpdate, PosteriorUpdate};
use calibration::{CalibrationConfig, CalibrationEngine};
use calibration::state::CalibrationState;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Unit tests — CalibrationState
// ---------------------------------------------------------------------------

/// Predict 0.8, market resolves YES (prob >= 0.98) → outcome=1.0, brier=0.04.
#[test]
fn posterior_then_resolve_yes() {
    let config = CalibrationConfig::default();
    let mut state = CalibrationState::new(config);

    state.on_posterior("mkt-1", 0.8, Utc::now());
    let record = state.on_market_update("mkt-1", 0.99).unwrap();

    assert_eq!(record.outcome, 1.0);
    assert!((record.brier_score - 0.04).abs() < 1e-9);
    assert_eq!(record.market_id, "mkt-1");
    assert!((record.predicted_prob - 0.8).abs() < 1e-9);
}

/// Predict 0.3, market resolves NO (prob <= 0.02) → outcome=0.0, brier=0.09.
#[test]
fn posterior_then_resolve_no() {
    let config = CalibrationConfig::default();
    let mut state = CalibrationState::new(config);

    state.on_posterior("mkt-2", 0.3, Utc::now());
    let record = state.on_market_update("mkt-2", 0.01).unwrap();

    assert_eq!(record.outcome, 0.0);
    assert!((record.brier_score - 0.09).abs() < 1e-9);
}

/// Resolving a market with no prior prediction returns None.
#[test]
fn no_pending_prediction_returns_none() {
    let config = CalibrationConfig::default();
    let mut state = CalibrationState::new(config);

    let result = state.on_market_update("no-prediction", 0.99);
    assert!(result.is_none());
}

/// Resolving the same market twice — second call returns None.
#[test]
fn duplicate_resolution_ignored() {
    let config = CalibrationConfig::default();
    let mut state = CalibrationState::new(config);

    state.on_posterior("mkt-dup", 0.5, Utc::now());
    let first = state.on_market_update("mkt-dup", 0.99);
    assert!(first.is_some());

    // Second resolution of the same market.
    let second = state.on_market_update("mkt-dup", 0.99);
    assert!(second.is_none());
}

/// With no records, overall_brier returns the uninformative prior of 0.5.
#[test]
fn overall_brier_uninformative() {
    let config = CalibrationConfig::default();
    let state = CalibrationState::new(config);

    assert!((state.overall_brier() - 0.5).abs() < 1e-9);
}

/// Multiple predictions resolved — verify mean Brier score.
#[test]
fn overall_brier_multiple_records() {
    let config = CalibrationConfig::default();
    let mut state = CalibrationState::new(config);

    // Prediction 1: predict 0.8, resolves YES → brier = 0.04
    state.on_posterior("m1", 0.8, Utc::now());
    state.on_market_update("m1", 0.99);

    // Prediction 2: predict 0.3, resolves NO → brier = 0.09
    state.on_posterior("m2", 0.3, Utc::now());
    state.on_market_update("m2", 0.01);

    // Prediction 3: predict 0.5, resolves YES → brier = 0.25
    state.on_posterior("m3", 0.5, Utc::now());
    state.on_market_update("m3", 0.99);

    // Mean = (0.04 + 0.09 + 0.25) / 3 = 0.38 / 3 ≈ 0.12666...
    let expected = (0.04 + 0.09 + 0.25) / 3.0;
    assert!((state.overall_brier() - expected).abs() < 1e-9);
}

/// Verify that predictions land in the correct calibration bucket.
#[test]
fn calibration_buckets() {
    let config = CalibrationConfig {
        num_buckets: 10,
        ..CalibrationConfig::default()
    };
    let mut state = CalibrationState::new(config);

    // Predict 0.15 → bucket index 1 (bin [0.1, 0.2)), resolves NO
    state.on_posterior("b1", 0.15, Utc::now());
    state.on_market_update("b1", 0.001);

    // Predict 0.85 → bucket index 8 (bin [0.8, 0.9)), resolves YES
    state.on_posterior("b2", 0.85, Utc::now());
    state.on_market_update("b2", 0.999);

    let buckets = state.compute_buckets();
    assert_eq!(buckets.len(), 10);

    // Bucket 1 (center 0.15): 1 record, predicted 0.15, resolved NO (0.0)
    assert_eq!(buckets[1].count, 1);
    assert!((buckets[1].mean_predicted - 0.15).abs() < 1e-9);
    assert!((buckets[1].fraction_positive - 0.0).abs() < 1e-9);

    // Bucket 8 (center 0.85): 1 record, predicted 0.85, resolved YES (1.0)
    assert_eq!(buckets[8].count, 1);
    assert!((buckets[8].mean_predicted - 0.85).abs() < 1e-9);
    assert!((buckets[8].fraction_positive - 1.0).abs() < 1e-9);

    // All other buckets should be empty.
    for (i, b) in buckets.iter().enumerate() {
        if i != 1 && i != 8 {
            assert_eq!(b.count, 0, "bucket {i} should be empty");
        }
    }
}

// ---------------------------------------------------------------------------
// Integration test — CalibrationEngine via EventBus
// ---------------------------------------------------------------------------

/// Publish Event::Posterior then Event::Market(resolved), verify
/// Event::CalibrationUpdate is emitted on the bus.
#[tokio::test]
async fn engine_integration_posterior_then_resolve() {
    let bus = EventBus::new();
    let mut rx = bus.subscribe();

    let engine = CalibrationEngine::new(CalibrationConfig::default(), bus.clone());
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move { engine.run(c).await });

    tokio::task::yield_now().await;

    // Publish a posterior prediction.
    let posterior = PosteriorUpdate {
        market_id:      "int-mkt".to_string(),
        prior_prob:     0.5,
        posterior_prob:  0.75,
        market_prob:    Some(0.5),
        deviation:      0.25,
        confidence:     0.9,
        timestamp:      Utc::now(),
    };
    bus.publish(Event::Posterior(posterior)).unwrap();

    // Small yield to let the engine process the posterior.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Publish a market resolution (price → 0.99, within epsilon of 1.0 → YES).
    let market_update = MarketUpdate {
        market: MarketNode {
            id:          "int-mkt".to_string(),
            probability: 0.99,
            liquidity:   1000.0,
            last_update: Utc::now(),
        },
    };
    bus.publish(Event::Market(market_update)).unwrap();

    // Wait for CalibrationUpdate.
    let update = timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(ev) = rx.recv().await {
                if let Event::CalibrationUpdate(cu) = ev.as_ref() {
                    return cu.clone();
                }
            }
        }
    })
    .await
    .expect("timed out waiting for CalibrationUpdate");

    assert_eq!(update.market_id, "int-mkt");
    assert!((update.predicted_prob - 0.75).abs() < 1e-9);
    assert_eq!(update.outcome, 1.0);
    // brier = (0.75 - 1.0)^2 = 0.0625
    assert!((update.brier_score - 0.0625).abs() < 1e-9);

    cancel.cancel();
}
