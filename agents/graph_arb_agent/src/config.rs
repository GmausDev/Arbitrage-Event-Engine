// agents/graph_arb_agent/src/config.rs

use serde::{Deserialize, Serialize};

/// Configuration for the Graph Arbitrage Agent.
///
/// Emits a `TradeSignal` when the BFS-propagated graph-implied probability
/// for a market deviates from its observed market price by more than
/// `min_edge_threshold` **and** the resulting EV exceeds trading costs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphArbConfig {
    /// Minimum |implied_prob − market_prob| required to emit a signal.
    /// Default: 0.04 (4 percentage points).
    pub min_edge_threshold: f64,

    /// Round-trip trading cost (slippage + fees) in fractional terms.
    /// Default: 0.003 (30 bps).
    pub trading_cost: f64,

    /// Maximum position fraction of bankroll per signal.
    /// Default: 0.05.
    pub max_position_fraction: f64,

    /// Kelly fraction multiplier (quarter-Kelly by default).
    /// Default: 0.25.
    pub kelly_fraction: f64,

    /// Scale factor for confidence: `(|delta| / confidence_scale).clamp(0, 1)`.
    /// A delta equal to `confidence_scale` gives confidence = 1.0.
    /// Default: 0.20 (20 percentage points → full confidence).
    pub confidence_scale: f64,

    /// Lower price bound: suppress signals for markets priced below this.
    /// Default: 0.05.
    pub min_market_price: f64,

    /// Upper price bound: suppress signals for markets priced above this.
    /// Default: 0.95.
    pub max_market_price: f64,

    // ── Cross-platform arbitrage ───────────────────────────────────────────

    /// Minimum Jaccard title-similarity score (word-token overlap) for two
    /// markets to be considered the same real-world event across platforms.
    /// Default: 0.40 (40 % word overlap — enough for "Will X win?" phrasing
    /// differences between Polymarket and Kalshi).
    pub xplatform_min_title_similarity: f64,

    /// Minimum raw probability spread required between the two platforms.
    /// Default: 0.02 (2 percentage points).
    pub xplatform_min_spread: f64,

    /// One-way trading cost on each platform (slippage + fees).
    /// The net EV is `spread − 2 × xplatform_trading_cost`.
    /// Default: 0.008 (80 bps per side → 160 bps round-trip).
    pub xplatform_trading_cost: f64,

    /// Maximum position fraction per cross-platform signal.
    /// Kept smaller than `max_position_fraction` because simultaneous
    /// execution on two platforms carries more implementation risk.
    /// Default: 0.03.
    pub xplatform_max_position_fraction: f64,

    /// Minimum seconds between re-emitting a signal for the same
    /// cross-platform pair, to avoid flooding the bus on every tick.
    /// Default: 60.
    pub xplatform_signal_cooldown_secs: u64,
}

impl Default for GraphArbConfig {
    fn default() -> Self {
        Self {
            min_edge_threshold: 0.04,
            trading_cost: 0.003,
            max_position_fraction: 0.05,
            kelly_fraction: 0.25,
            confidence_scale: 0.20,
            min_market_price: 0.05,
            max_market_price: 0.95,
            xplatform_min_title_similarity: 0.40,
            xplatform_min_spread: 0.02,
            xplatform_trading_cost: 0.008,
            xplatform_max_position_fraction: 0.03,
            xplatform_signal_cooldown_secs: 60,
        }
    }
}

impl GraphArbConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !self.min_edge_threshold.is_finite() || self.min_edge_threshold <= 0.0 {
            return Err("min_edge_threshold must be positive and finite".into());
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
        if !self.confidence_scale.is_finite() || self.confidence_scale <= 0.0 {
            return Err("confidence_scale must be positive and finite".into());
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
        if !self.xplatform_min_title_similarity.is_finite()
            || !(0.0..=1.0).contains(&self.xplatform_min_title_similarity)
        {
            return Err("xplatform_min_title_similarity must be in [0, 1]".into());
        }
        if !self.xplatform_min_spread.is_finite() || self.xplatform_min_spread < 0.0 {
            return Err("xplatform_min_spread must be non-negative and finite".into());
        }
        if !self.xplatform_trading_cost.is_finite() || self.xplatform_trading_cost < 0.0 {
            return Err("xplatform_trading_cost must be non-negative and finite".into());
        }
        if !self.xplatform_max_position_fraction.is_finite()
            || !(0.0..=1.0).contains(&self.xplatform_max_position_fraction)
        {
            return Err("xplatform_max_position_fraction must be in [0, 1]".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        assert!(GraphArbConfig::default().validate().is_ok());
    }

    #[test]
    fn zero_edge_threshold_rejected() {
        let cfg = GraphArbConfig {
            min_edge_threshold: 0.0,
            ..GraphArbConfig::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn nan_trading_cost_rejected() {
        let cfg = GraphArbConfig {
            trading_cost: f64::NAN,
            ..GraphArbConfig::default()
        };
        assert!(cfg.validate().is_err());
    }
}
