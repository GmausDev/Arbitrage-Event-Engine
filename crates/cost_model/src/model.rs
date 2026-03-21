// crates/cost_model/src/model.rs

use crate::config::CostModelConfig;

// ── Cost breakdown ────────────────────────────────────────────────────────────

/// Full breakdown of estimated trading costs for a single signal/trade.
///
/// All fields are expressed on the same scale as the gross edge (0.0–1.0
/// probability units), so `net_edge = gross_edge − total_cost` is a
/// dimensionally-consistent subtraction.
#[derive(Debug, Clone)]
pub struct CostEstimate {
    /// Exchange/platform fee (`fee_rate`).
    pub fees: f64,
    /// Bid-ask spread approximation (`spread_volatility_k × volatility`).
    pub spread: f64,
    /// Linear market-impact slippage (`position_fraction × liquidity_impact_factor`).
    pub slippage: f64,
    /// Edge eroded by execution latency:
    /// `gross_edge × (1 − e^{−decay_rate × latency_ms})`.
    pub decay_cost: f64,
    /// Sum of all cost components.
    pub total_cost: f64,
}

// ── Core computation functions ────────────────────────────────────────────────

/// Compute the full cost breakdown for a potential trade.
///
/// # Arguments
/// * `gross_edge`        — `|posterior_prob − market_prob|` in probability
///                         units (0..1).
/// * `position_fraction` — Signal position size as a fraction of bankroll.
/// * `volatility`        — Market volatility in probability units; use
///                         `config.default_volatility` when not observed.
/// * `latency_ms`        — Observed (or expected) execution latency in ms.
/// * `config`            — Cost model parameters.
pub fn compute_cost_estimate(
    gross_edge: f64,
    position_fraction: f64,
    volatility: f64,
    latency_ms: f64,
    config: &CostModelConfig,
) -> CostEstimate {
    let fees       = config.fee_rate;
    let spread     = config.spread_volatility_k * volatility;
    let slippage   = position_fraction * config.liquidity_impact_factor;
    let decay_cost = gross_edge * (1.0 - (-config.decay_rate * latency_ms).exp());
    let total_cost = fees + spread + slippage + decay_cost;

    CostEstimate { fees, spread, slippage, decay_cost, total_cost }
}

/// `net_edge = gross_edge − total_cost`, clamped to [−1, 1].
#[inline]
pub fn compute_net_edge(gross_edge: f64, cost: &CostEstimate) -> f64 {
    (gross_edge - cost.total_cost).clamp(-1.0, 1.0)
}

/// Expected dollar profit from a trade: `net_edge × position_size_usd`.
#[inline]
pub fn compute_expected_profit(net_edge: f64, position_size_usd: f64) -> f64 {
    net_edge * position_size_usd
}

/// Scale a base position fraction by the net-to-gross edge ratio.
///
/// Reduces the position proportionally when costs erode part of the edge,
/// so Kelly sizing stays economically consistent with the realised net edge.
///
/// Returns 0.0 when `gross_edge ≤ 0` (avoids division by zero).
#[inline]
pub fn compute_net_edge_position_size(
    base_fraction: f64,
    net_edge: f64,
    gross_edge: f64,
) -> f64 {
    if gross_edge <= 0.0 {
        return 0.0;
    }
    base_fraction * (net_edge / gross_edge).clamp(0.0, 1.0)
}
