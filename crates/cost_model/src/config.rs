// crates/cost_model/src/config.rs

use serde::{Deserialize, Serialize};

/// Configuration for the trading cost model.
///
/// All fractional fields (fee_rate, spread coefficients) are on the same
/// scale as edge values (0.0–1.0 probability units).  Dollar-denominated
/// thresholds (min_expected_profit_usd) are in USD.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CostModelConfig {
    /// Exchange/platform fee as a fraction of position value.
    ///
    /// Applied as a round-trip cost at entry (exit fee is assumed symmetric).
    /// Default: 0.02 (2 %).
    pub fee_rate: f64,

    /// Spread-cost multiplier: `spread_cost = spread_volatility_k × volatility`.
    ///
    /// A value of 0.5 means the spread cost is half the market's one-sigma
    /// move in probability space.  Default: 0.5.
    pub spread_volatility_k: f64,

    /// Linear market-impact coefficient:
    /// `slippage = position_fraction × liquidity_impact_factor`.
    ///
    /// Default: 0.001 — a 5 % position incurs 5 bps of slippage.
    pub liquidity_impact_factor: f64,

    /// Edge-decay rate per millisecond of execution latency.
    ///
    /// `decay_cost = gross_edge × (1 − e^{−λ × latency_ms})`
    ///
    /// Default: 0.001 (half-life ≈ 693 ms).
    pub decay_rate: f64,

    /// Baseline execution latency used before any `EdgeDecayReport` is
    /// observed.  Replaced by the EMA of observed latencies over time.
    ///
    /// Default: 50.0 ms.
    pub expected_latency_ms: f64,

    /// Minimum expected net profit in USD required to approve a trade.
    ///
    /// Guards against signals with positive net edge but negligibly small
    /// position sizes that would yield sub-threshold absolute gains.
    ///
    /// Default: $0.50.
    pub min_expected_profit_usd: f64,

    /// Fallback market volatility (probability units) used when no recent
    /// market data is available.  Default: 0.02.
    pub default_volatility: f64,

    /// Fallback market liquidity in USD used when no market data is cached.
    /// Default: 5 000.0.
    pub default_market_liquidity: f64,
}

impl Default for CostModelConfig {
    fn default() -> Self {
        Self {
            fee_rate:                 0.02,
            spread_volatility_k:      0.5,
            liquidity_impact_factor:  0.001,
            decay_rate:               0.001,
            expected_latency_ms:      50.0,
            min_expected_profit_usd:  0.50,
            default_volatility:       0.02,
            default_market_liquidity: 5_000.0,
        }
    }
}
