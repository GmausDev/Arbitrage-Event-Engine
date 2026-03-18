// crates/shock_detector/src/config.rs

use serde::{Deserialize, Serialize};

/// Configuration for the Information Shock Detector.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShockDetectorConfig {
    /// Number of historical price-delta samples kept per market for z-score
    /// computation.  Larger windows yield more stable baselines but react
    /// more slowly to genuine regime shifts.
    ///
    /// **Warm-up**: a market must accumulate at least 3 `MarketUpdate` events
    /// before z-scoring can produce a result (2 real deltas required for
    /// sample stddev).  Shocks during this warm-up period are silently
    /// suppressed.  The effective warm-up count is `min(3, rolling_window)`.
    pub rolling_window: usize,

    /// Z-score threshold above which a price move is classified as a shock.
    /// Typical values: 2.0 (sensitive) to 3.0 (conservative).
    pub z_score_threshold: f64,

    /// Absolute sentiment score threshold in [0, 1] for a `SentimentUpdate`
    /// to be classified as a shock on its own.
    pub sentiment_threshold: f64,

    /// Weight applied to the (normalised) price z-score component when fusing
    /// price + sentiment signals.  The sentiment weight is `1 - price_weight`.
    /// Applies only when both signals are active for the same market.
    pub price_weight: f64,

    /// Maximum magnitude of a z-score normalised to [0, 1].
    /// z-scores at or above `z_score_threshold * z_scale_factor` yield
    /// magnitude = 1.0.
    pub z_scale_factor: f64,

    /// Number of seconds a sentiment score remains "fresh" for fusion with a
    /// price shock.  After this window, the old sentiment is ignored.
    pub sentiment_freshness_secs: u64,

    /// Maximum number of distinct markets tracked simultaneously.
    /// When the limit is reached and a new market is seen, the market with
    /// the oldest last-seen timestamp is evicted to make room.
    /// Set to 0 to disable the cap (unbounded growth).
    pub max_tracked_markets: usize,
}

impl Default for ShockDetectorConfig {
    fn default() -> Self {
        Self {
            rolling_window: 20,
            z_score_threshold: 2.5,
            sentiment_threshold: 0.4,
            price_weight: 0.7,
            z_scale_factor: 3.0,
            sentiment_freshness_secs: 60,
            max_tracked_markets: 10_000,
        }
    }
}

impl ShockDetectorConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.rolling_window < 2 {
            return Err("rolling_window must be >= 2".to_string());
        }
        // Use is_finite() so that Infinity is also rejected in addition to NaN.
        if !self.z_score_threshold.is_finite() || self.z_score_threshold <= 0.0 {
            return Err(format!(
                "z_score_threshold must be finite and > 0, got {}",
                self.z_score_threshold
            ));
        }
        if !self.sentiment_threshold.is_finite()
            || self.sentiment_threshold < 0.0
            || self.sentiment_threshold > 1.0
        {
            return Err(format!(
                "sentiment_threshold must be finite and in [0, 1], got {}",
                self.sentiment_threshold
            ));
        }
        if !self.price_weight.is_finite() || self.price_weight < 0.0 || self.price_weight > 1.0 {
            return Err(format!(
                "price_weight must be finite and in [0, 1], got {}",
                self.price_weight
            ));
        }
        if !self.z_scale_factor.is_finite() || self.z_scale_factor <= 0.0 {
            return Err(format!(
                "z_scale_factor must be finite and > 0, got {}",
                self.z_scale_factor
            ));
        }
        Ok(())
    }
}
