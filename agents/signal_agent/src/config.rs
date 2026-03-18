// agents/signal_agent/src/config.rs
// Configuration for the Signal Agent with serde-default support.

use serde::{Deserialize, Serialize};

/// Configuration for the Signal Agent.
///
/// All fields have production-safe defaults; override via TOML config.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SignalAgentConfig {
    /// Minimum expected value (after trading costs) required to emit a signal.
    /// Default: 0.02 (2%).
    pub min_expected_value: f64,

    /// Hard cap on the Kelly-sized position fraction (fraction of bankroll).
    /// Default: 0.05 (5%).
    pub max_position_fraction: f64,

    /// Fractional Kelly multiplier applied before the hard cap.
    /// Default: 0.25 (quarter-Kelly).
    pub kelly_fraction: f64,

    /// Markets with price > this value are skipped (near-certain outcomes).
    /// Default: 0.95.
    pub max_market_price: f64,

    /// Markets with price < this value are skipped (near-impossible outcomes).
    /// Default: 0.05.
    pub min_market_price: f64,

    /// Minimum model confidence to emit a signal.
    /// Default: 0.30.
    pub min_confidence: f64,

    /// One-way slippage estimate in basis points.
    /// Default: 20 bps.
    pub slippage_bps: f64,

    /// One-way trading fee in basis points.
    /// Default: 10 bps.
    pub fee_bps: f64,
}

impl Default for SignalAgentConfig {
    fn default() -> Self {
        Self {
            min_expected_value: 0.02,
            max_position_fraction: 0.05,
            kelly_fraction: 0.25,
            max_market_price: 0.95,
            min_market_price: 0.05,
            min_confidence: 0.30,
            slippage_bps: 20.0,
            fee_bps: 10.0,
        }
    }
}

impl SignalAgentConfig {
    /// Total round-trip trading cost as a probability fraction.
    ///
    /// `(slippage_bps + fee_bps) / 10_000`
    pub fn trading_cost(&self) -> f64 {
        (self.slippage_bps + self.fee_bps) / 10_000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trading_cost_is_correct() {
        let cfg = SignalAgentConfig::default(); // 20 + 10 = 30 bps = 0.003
        assert!((cfg.trading_cost() - 0.003).abs() < 1e-12);
    }
}
