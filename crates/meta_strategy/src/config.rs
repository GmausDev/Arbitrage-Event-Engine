// crates/meta_strategy/src/config.rs

use serde::{Deserialize, Serialize};

/// Configuration for the `MetaStrategyEngine`.
///
/// All thresholds are checked by `validate()` at construction time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaStrategyConfig {
    /// Minimum combined directional confidence to emit a MetaSignal [0, 1].
    ///
    /// Combined confidence is the margin of directional agreement, weighted
    /// by each signal's `confidence × expected_value`.  A value of 0.0 means
    /// the strategies are perfectly split; 1.0 means unanimous.
    pub min_combined_confidence: f64,

    /// Minimum confidence-weighted mean EV across contributing signals.
    pub min_expected_edge: f64,

    /// Multiplier applied to combined confidence when a fresh shock exists:
    ///   boosted = combined_confidence × (1 + shock_boost_factor × magnitude)
    pub shock_boost_factor: f64,

    /// Maximum age of an `InformationShock` to be considered "fresh", in seconds.
    pub shock_freshness_secs: u64,

    /// Maximum age of an individual `TradeSignal` to be included in aggregation.
    /// Signals older than this are silently discarded.
    pub signal_ttl_secs: u64,

    /// Minimum number of distinct strategies required to form a MetaSignal.
    /// Set to 1 to allow single-strategy signals through; set to 2+ to require
    /// corroboration from multiple independent sources.
    pub min_strategies: usize,
}

impl Default for MetaStrategyConfig {
    fn default() -> Self {
        Self {
            min_combined_confidence: 0.30,
            min_expected_edge:       0.005,
            shock_boost_factor:      0.25,
            shock_freshness_secs:    300,
            signal_ttl_secs:         60,
            min_strategies:          1,
        }
    }
}

impl MetaStrategyConfig {
    /// Returns `Err` with a human-readable message if any field is invalid.
    pub fn validate(&self) -> Result<(), String> {
        if !self.min_combined_confidence.is_finite()
            || !(0.0..=1.0).contains(&self.min_combined_confidence)
        {
            return Err(format!(
                "min_combined_confidence must be in [0, 1], got {}",
                self.min_combined_confidence
            ));
        }
        if !self.min_expected_edge.is_finite() || self.min_expected_edge < 0.0 {
            return Err(format!(
                "min_expected_edge must be ≥ 0, got {}",
                self.min_expected_edge
            ));
        }
        if !self.shock_boost_factor.is_finite() || self.shock_boost_factor < 0.0 {
            return Err(format!(
                "shock_boost_factor must be ≥ 0, got {}",
                self.shock_boost_factor
            ));
        }
        if self.min_strategies == 0 {
            return Err("min_strategies must be ≥ 1".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        assert!(MetaStrategyConfig::default().validate().is_ok());
    }

    #[test]
    fn confidence_above_one_rejected() {
        let cfg = MetaStrategyConfig {
            min_combined_confidence: 1.1,
            ..Default::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn negative_edge_rejected() {
        let cfg = MetaStrategyConfig {
            min_expected_edge: -0.01,
            ..Default::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn zero_min_strategies_rejected() {
        let cfg = MetaStrategyConfig {
            min_strategies: 0,
            ..Default::default()
        };
        assert!(cfg.validate().is_err());
    }
}
