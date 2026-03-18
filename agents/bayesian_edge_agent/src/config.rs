// agents/bayesian_edge_agent/src/config.rs

use serde::{Deserialize, Serialize};

/// Configuration for the Bayesian Edge Agent.
///
/// Like `signal_agent` but with two additional fusion features:
///
/// 1. **Shock boost** — when a fresh `InformationShock` exists for a market,
///    the Bayesian confidence is multiplied by `(1 + shock_boost_factor × magnitude)`.
///
/// 2. **Graph damping** — when a graph-implied probability exists, the posterior
///    is blended toward it: `posterior_adj = (1−graph_damp) × posterior + graph_damp × implied`.
///    This reduces the signal when the graph disagrees with the Bayesian model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BayesianEdgeConfig {
    /// Minimum Bayesian confidence (after shock boost) required to emit a signal.
    /// Default: 0.25.
    pub min_confidence: f64,

    /// Minimum expected value after trading costs.
    /// Default: 0.015.
    pub min_expected_value: f64,

    /// Round-trip trading cost in fractional terms.
    /// Default: 0.003 (30 bps).
    pub trading_cost: f64,

    /// Maximum position fraction of bankroll per signal.
    /// Default: 0.05.
    pub max_position_fraction: f64,

    /// Kelly fraction multiplier.
    /// Default: 0.25.
    pub kelly_fraction: f64,

    /// Shock confidence boost factor.
    /// `boosted_conf = min(1, base_conf × (1 + shock_boost_factor × magnitude))`.
    /// Default: 0.5.
    pub shock_boost_factor: f64,

    /// Maximum age of a shock (in seconds) for it to still boost confidence.
    /// Default: 300 (5 minutes).
    pub shock_freshness_secs: u64,

    /// Graph damping weight toward the graph-implied probability [0, 1].
    /// 0 = pure Bayesian posterior; 1 = pure graph-implied.
    /// Default: 0.20.
    pub graph_damp_factor: f64,

    /// Lower price bound: suppress signals for markets priced below this.
    /// Default: 0.05.
    pub min_market_price: f64,

    /// Upper price bound: suppress signals for markets priced above this.
    /// Default: 0.95.
    pub max_market_price: f64,
}

impl Default for BayesianEdgeConfig {
    fn default() -> Self {
        Self {
            min_confidence: 0.25,
            min_expected_value: 0.015,
            trading_cost: 0.003,
            max_position_fraction: 0.05,
            kelly_fraction: 0.25,
            shock_boost_factor: 0.5,
            shock_freshness_secs: 300,
            graph_damp_factor: 0.20,
            min_market_price: 0.05,
            max_market_price: 0.95,
        }
    }
}

impl BayesianEdgeConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !self.min_confidence.is_finite() || !(0.0..=1.0).contains(&self.min_confidence) {
            return Err("min_confidence must be in [0, 1]".into());
        }
        if !self.min_expected_value.is_finite() || self.min_expected_value < 0.0 {
            return Err("min_expected_value must be non-negative and finite".into());
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
        if !self.shock_boost_factor.is_finite() || self.shock_boost_factor < 0.0 {
            return Err("shock_boost_factor must be non-negative and finite".into());
        }
        if !self.graph_damp_factor.is_finite() || !(0.0..=1.0).contains(&self.graph_damp_factor) {
            return Err("graph_damp_factor must be in [0, 1]".into());
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
        assert!(BayesianEdgeConfig::default().validate().is_ok());
    }

    #[test]
    fn nan_confidence_rejected() {
        let cfg = BayesianEdgeConfig {
            min_confidence: f64::NAN,
            ..BayesianEdgeConfig::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn graph_damp_above_one_rejected() {
        let cfg = BayesianEdgeConfig {
            graph_damp_factor: 1.5,
            ..BayesianEdgeConfig::default()
        };
        assert!(cfg.validate().is_err());
    }
}
