// agents/temporal_agent/src/config.rs

use serde::{Deserialize, Serialize};

/// Configuration for the Temporal Strategy Agent.
///
/// Detects momentum by computing the z-score of the current market price
/// relative to its rolling history.  A signal is emitted when
/// `|z| >= trend_z_threshold`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemporalConfig {
    /// Number of price samples to retain per market.
    /// Default: 20.
    pub history_window: usize,

    /// Minimum history length before signals are considered.
    /// Default: 5.
    pub min_history: usize,

    /// Z-score threshold to trigger a signal.
    /// Default: 1.5.
    pub trend_z_threshold: f64,

    /// Denominator for confidence: `(|z| / trend_z_scale).clamp(0, 1)`.
    /// A z-score equal to `trend_z_scale` gives confidence = 1.0.
    /// Default: 3.0.
    pub trend_z_scale: f64,

    /// Round-trip trading cost in fractional terms.
    /// Default: 0.003 (30 bps).
    pub trading_cost: f64,

    /// Maximum position fraction of bankroll per signal.
    /// Default: 0.03 (smaller than EV agents; trend signals are weaker).
    pub max_position_fraction: f64,

    /// Kelly fraction multiplier.
    /// Default: 0.25.
    pub kelly_fraction: f64,

    /// Lower price bound: suppress signals for markets priced below this.
    /// Default: 0.05.
    pub min_market_price: f64,

    /// Upper price bound: suppress signals for markets priced above this.
    /// Default: 0.95.
    pub max_market_price: f64,
}

impl Default for TemporalConfig {
    fn default() -> Self {
        Self {
            history_window: 20,
            min_history: 5,
            trend_z_threshold: 1.0,
            trend_z_scale: 3.0,
            trading_cost: 0.003,
            max_position_fraction: 0.03,
            kelly_fraction: 0.25,
            min_market_price: 0.05,
            max_market_price: 0.95,
        }
    }
}

impl TemporalConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.history_window < 2 {
            return Err("history_window must be >= 2".into());
        }
        if self.min_history < 2 {
            return Err("min_history must be >= 2".into());
        }
        if self.min_history > self.history_window {
            return Err("min_history must be <= history_window".into());
        }
        if !self.trend_z_threshold.is_finite() || self.trend_z_threshold <= 0.0 {
            return Err("trend_z_threshold must be positive and finite".into());
        }
        if !self.trend_z_scale.is_finite() || self.trend_z_scale <= 0.0 {
            return Err("trend_z_scale must be positive and finite".into());
        }
        if !self.trading_cost.is_finite() || self.trading_cost < 0.0 {
            return Err("trading_cost must be non-negative and finite".into());
        }
        if !self.max_position_fraction.is_finite()
            || !(0.0..=1.0).contains(&self.max_position_fraction)
        {
            return Err("max_position_fraction must be in [0, 1]".into());
        }
        if !self.kelly_fraction.is_finite() || !(0.0..=1.0).contains(&self.kelly_fraction) {
            return Err("kelly_fraction must be in [0, 1]".into());
        }
        if !self.min_market_price.is_finite() || self.min_market_price < 0.0 {
            return Err("min_market_price must be non-negative and finite".into());
        }
        if !self.max_market_price.is_finite() || self.max_market_price > 1.0 {
            return Err("max_market_price must be <= 1.0 and finite".into());
        }
        if self.min_market_price >= self.max_market_price {
            return Err("min_market_price must be < max_market_price".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        assert!(TemporalConfig::default().validate().is_ok());
    }

    #[test]
    fn min_history_exceeds_window_rejected() {
        let cfg = TemporalConfig {
            history_window: 5,
            min_history: 10,
            ..TemporalConfig::default()
        };
        assert!(cfg.validate().is_err());
    }
}
