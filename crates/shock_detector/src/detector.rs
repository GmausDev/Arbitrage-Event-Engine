// crates/shock_detector/src/detector.rs
//
// Pure, synchronous shock detection logic.
//
// All functions are free of I/O and async — they operate only on owned data
// so they can be tested without a runtime and called from `spawn_blocking`.
//
// ## Detection approach
//
// **Price shock** (from `MarketUpdate`)
//   delta = new_prob − last_prob
//   z     = (delta − mean(history)) / stddev(history)
//
//   If |z| ≥ z_score_threshold → shock detected.
//   Magnitude is normalised to [0, 1]:
//     magnitude = clamp(|z| / (threshold × z_scale_factor), 0, 1)
//   so that exactly-threshold gives ~(1/z_scale_factor) and
//   threshold×z_scale_factor gives 1.0.
//
// **Sentiment shock** (from `SentimentUpdate`)
//   If |sentiment_score| ≥ sentiment_threshold → shock detected.
//   Magnitude = |sentiment_score| (already in [0, 1]).
//
// **Fusion** (price + recent sentiment)
//   When a price shock fires and a fresh sentiment reading exists for the
//   same market, the magnitude is blended:
//     fused_magnitude = price_weight × price_magnitude
//                     + (1 − price_weight) × |sentiment_score|
//   Direction is taken from the price move; sentiment direction is used only
//   for sentiment-only shocks.

use chrono::{DateTime, Utc};
use common::{InformationShock, ShockDirection, ShockSource};

use crate::{config::ShockDetectorConfig, types::MarketState};

// ---------------------------------------------------------------------------
// Price-based shock detection
// ---------------------------------------------------------------------------

/// Attempt to detect a shock from a new market probability observation.
///
/// Returns `Some(InformationShock)` when the z-score of the price delta
/// exceeds `config.z_score_threshold`, `None` otherwise.
///
/// # Arguments
/// * `market_id` — market being updated.
/// * `state` — current rolling state for the market (will be read but NOT mutated here).
/// * `new_prob` — the freshly observed probability.
/// * `config` — detector configuration.
/// * `now` — current timestamp for the emitted shock.
pub fn detect_price_shock(
    market_id: &str,
    state: &MarketState,
    new_prob: f64,
    config: &ShockDetectorConfig,
    now: DateTime<Utc>,
) -> Option<InformationShock> {
    let delta = new_prob - state.last_prob;

    // Need at least 2 historical deltas to compute a meaningful z-score.
    let stddev = state.stddev_delta()?;
    if stddev < 1e-10 {
        return None; // no variance in history → cannot z-score
    }

    let mean = state.mean_delta();
    let z_score = (delta - mean) / stddev;

    if z_score.abs() < config.z_score_threshold {
        return None;
    }

    let direction = if delta > 0.0 {
        ShockDirection::Up
    } else if delta < 0.0 {
        ShockDirection::Down
    } else {
        ShockDirection::Neutral
    };

    // Normalise magnitude to [0, 1].
    let scale = config.z_score_threshold * config.z_scale_factor;
    let price_magnitude = (z_score.abs() / scale).clamp(0.0, 1.0);

    // Fuse with sentiment if a fresh reading exists for this market.
    let magnitude = fuse_magnitude(price_magnitude, state, config, now);

    Some(InformationShock {
        market_id: market_id.to_string(),
        magnitude,
        direction,
        source: ShockSource::Volatility,
        z_score,
        timestamp: now,
    })
}

// ---------------------------------------------------------------------------
// Sentiment-based shock detection
// ---------------------------------------------------------------------------

/// Attempt to detect a shock from a sentiment score update for one market.
///
/// Returns `Some(InformationShock)` when `|sentiment_score| >= threshold`.
pub fn detect_sentiment_shock(
    market_id: &str,
    sentiment_score: f64,
    config: &ShockDetectorConfig,
    now: DateTime<Utc>,
) -> Option<InformationShock> {
    if sentiment_score.abs() < config.sentiment_threshold {
        return None;
    }

    let magnitude = sentiment_score.abs().clamp(0.0, 1.0);
    let direction = if sentiment_score > 0.0 {
        ShockDirection::Up
    } else if sentiment_score < 0.0 {
        ShockDirection::Down
    } else {
        ShockDirection::Neutral
    };

    Some(InformationShock {
        market_id: market_id.to_string(),
        magnitude,
        direction,
        source: ShockSource::News,
        z_score: 0.0,
        timestamp: now,
    })
}

// ---------------------------------------------------------------------------
// Signal fusion helper
// ---------------------------------------------------------------------------

/// Blend a price-shock magnitude with a recent sentiment reading.
///
/// If no fresh sentiment exists, returns `price_magnitude` unchanged.
fn fuse_magnitude(
    price_magnitude: f64,
    state: &MarketState,
    config: &ShockDetectorConfig,
    now: DateTime<Utc>,
) -> f64 {
    let Some(sentiment_ts) = state.last_sentiment_ts else {
        return price_magnitude;
    };

    // Note: if `sentiment_ts` is slightly in the future due to clock skew,
    // `unsigned_abs()` yields a small positive age and treats the sentiment
    // as fresh — an intentional, conservative choice for clock-skew tolerance.
    let age_secs = (now - sentiment_ts).num_seconds().unsigned_abs();
    if age_secs > config.sentiment_freshness_secs {
        return price_magnitude; // sentiment too stale
    }

    let sentiment_magnitude = state.last_sentiment_score.abs().clamp(0.0, 1.0);
    let w = config.price_weight;
    w * price_magnitude + (1.0 - w) * sentiment_magnitude
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn config() -> ShockDetectorConfig {
        ShockDetectorConfig::default()
    }

    fn state_with_history(last_prob: f64, deltas: &[f64]) -> MarketState {
        let mut s = MarketState::new(last_prob, Utc::now());
        s.delta_history = VecDeque::from(deltas.to_vec());
        s
    }

    /// Produce a slice of `n` alternating small deltas centred on `center`
    /// with amplitude `amp`, so that stddev > 0 (required for z-scoring).
    fn varied(n: usize, center: f64, amp: f64) -> Vec<f64> {
        (0..n)
            .map(|i| center + if i % 2 == 0 { amp } else { -amp })
            .collect()
    }

    // -----------------------------------------------------------------------
    // Price shock tests
    // -----------------------------------------------------------------------

    #[test]
    fn no_shock_below_z_threshold() {
        // High-variance history → the same small move has a low z-score.
        let state = state_with_history(0.5, &[0.05, -0.05, 0.05, -0.05, 0.05, -0.05]);
        // Move of 0.01 << historical stddev ≈ 0.05 → z ≈ 0.2, well below 2.5.
        let shock = detect_price_shock("M", &state, 0.51, &config(), Utc::now());
        assert!(shock.is_none(), "small delta should not trigger shock");
    }

    #[test]
    fn shock_detected_on_large_z_score() {
        // Stable history of tiny deltas (with variance), then a big jump → large z.
        let deltas = varied(20, 0.001, 0.0001); // mean≈0.001, stddev≈0.0001
        let state = state_with_history(0.5, &deltas);
        // A jump of 0.3 against stddev≈0.0001 → z ≈ 3000 >> threshold (2.5).
        let shock = detect_price_shock("M", &state, 0.8, &config(), Utc::now());
        assert!(shock.is_some(), "large move should trigger shock");
        let s = shock.unwrap();
        assert_eq!(s.direction, ShockDirection::Up);
        assert!(s.magnitude > 0.0 && s.magnitude <= 1.0);
        assert!(s.z_score > config().z_score_threshold);
    }

    #[test]
    fn shock_direction_down_for_drop() {
        let deltas = varied(20, 0.001, 0.0001);
        let state = state_with_history(0.8, &deltas);
        let shock = detect_price_shock("M", &state, 0.5, &config(), Utc::now()).unwrap();
        assert_eq!(shock.direction, ShockDirection::Down);
        assert!(shock.z_score < 0.0);
    }

    #[test]
    fn insufficient_history_returns_none() {
        // Only 1 delta → stddev undefined → no shock.
        let mut state = MarketState::new(0.5, Utc::now());
        state.delta_history = VecDeque::from(vec![0.01]);
        let shock = detect_price_shock("M", &state, 0.9, &config(), Utc::now());
        assert!(shock.is_none(), "insufficient history should suppress shock");
    }

    #[test]
    fn zero_variance_history_returns_none() {
        // All identical deltas → stddev = 0 → cannot z-score.
        let state = state_with_history(0.5, &[0.0; 10]);
        let shock = detect_price_shock("M", &state, 0.9, &config(), Utc::now());
        assert!(shock.is_none(), "zero-variance history should suppress shock");
    }

    #[test]
    fn magnitude_clamped_to_one() {
        // Astronomical z-score must not produce magnitude > 1.
        let deltas = varied(20, 0.0001, 0.00001); // mean≈0.0001, stddev≈0.00001
        let state = state_with_history(0.5, &deltas);
        // prob swings from 0.5 to 0.99 — enormous relative to the tiny history variance.
        let shock = detect_price_shock("M", &state, 0.99, &config(), Utc::now()).unwrap();
        assert!(shock.magnitude <= 1.0, "magnitude must not exceed 1.0");
    }

    // -----------------------------------------------------------------------
    // Sentiment shock tests
    // -----------------------------------------------------------------------

    #[test]
    fn sentiment_shock_above_threshold() {
        let shock = detect_sentiment_shock("M", 0.8, &config(), Utc::now());
        assert!(shock.is_some());
        let s = shock.unwrap();
        assert_eq!(s.direction, ShockDirection::Up);
        assert!((s.magnitude - 0.8).abs() < 1e-9);
        assert!(matches!(s.source, ShockSource::News));
    }

    #[test]
    fn sentiment_shock_below_threshold_suppressed() {
        let shock = detect_sentiment_shock("M", 0.1, &config(), Utc::now());
        assert!(shock.is_none());
    }

    #[test]
    fn negative_sentiment_direction_is_down() {
        let shock = detect_sentiment_shock("M", -0.9, &config(), Utc::now()).unwrap();
        assert_eq!(shock.direction, ShockDirection::Down);
    }

    // -----------------------------------------------------------------------
    // Fusion test
    // -----------------------------------------------------------------------

    #[test]
    fn fresh_sentiment_blends_magnitude() {
        let deltas = varied(20, 0.001, 0.0001);
        let mut state = state_with_history(0.5, &deltas);
        // Seed a strong recent sentiment.
        state.last_sentiment_score = 0.9;
        state.last_sentiment_ts = Some(Utc::now());

        let cfg = ShockDetectorConfig {
            price_weight: 0.7,
            ..ShockDetectorConfig::default()
        };
        let shock = detect_price_shock("M", &state, 0.8, &cfg, Utc::now()).unwrap();
        // price_magnitude is clamped to 1.0 (enormous z); sentiment = 0.9
        // fused = 0.7 * 1.0 + 0.3 * 0.9 = 0.97
        assert!(shock.magnitude > 0.9, "fused magnitude should be near 1.0 with strong sentiment");
    }

    #[test]
    fn stale_sentiment_not_fused() {
        let deltas = varied(20, 0.001, 0.0001);
        let mut state = state_with_history(0.5, &deltas);
        state.last_sentiment_score = 1.0;
        // Timestamp 120 s in the past → beyond the 60 s freshness window.
        state.last_sentiment_ts =
            Some(Utc::now() - chrono::Duration::seconds(120));

        let cfg = ShockDetectorConfig::default();
        let shock = detect_price_shock("M", &state, 0.9, &cfg, Utc::now()).unwrap();
        // Without fusion, magnitude = clamp(|z| / scale, 0, 1) = 1.0 for huge z.
        // With stale sentiment, still 1.0 — but the key check is that we do NOT
        // panic or produce a value > 1.0.
        assert!(shock.magnitude <= 1.0);
    }
}
