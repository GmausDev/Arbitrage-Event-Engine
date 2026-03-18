// crates/performance_analytics/src/types.rs

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// PortfolioMetrics — one snapshot in time
// ---------------------------------------------------------------------------

/// Performance snapshot captured after each `PortfolioUpdate`.
///
/// `per_market_pnl` is populated when a current market price is available in
/// the analytics engine's price cache (i.e. at least one `MarketUpdate` has
/// been received for that market).
///
/// `strategy_pnl` is reserved for when trade-level strategy tags flow through
/// the wire type; it is empty in v0.1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioMetrics {
    /// Total PnL in USD (realized + unrealized).
    pub total_pnl: f64,

    /// Estimated realized PnL: `total_pnl − computed unrealized across open positions`.
    ///
    /// Approximation: accurate when all open positions have a current price in
    /// the price cache; degrades gracefully when some prices are stale.
    pub realized_pnl: f64,

    /// Estimated unrealized PnL: sum of mark-to-market PnL for open positions.
    pub unrealized_pnl: f64,

    /// Total deployed capital in USD.
    pub exposure: f64,

    /// `exposure / initial_bankroll`.
    pub exposure_fraction: f64,

    /// Mark-to-market PnL per open market, keyed by `market_id`.
    /// Empty for markets with no price in the cache.
    pub per_market_pnl: HashMap<String, f64>,

    /// PnL attributed to named strategies.  Empty in v0.1 (requires tag
    /// propagation through the wire type).
    pub strategy_pnl: HashMap<String, f64>,

    /// Number of currently open positions.
    pub position_count: usize,

    /// `initial_bankroll + total_pnl`.
    pub equity: f64,

    pub timestamp: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// RiskMetrics — derived from the full history window
// ---------------------------------------------------------------------------

/// Risk-adjusted performance summary computed over the rolling history window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskMetrics {
    /// Annualised Sharpe ratio (risk-free rate assumed zero).
    ///
    /// `NAN` when fewer than 2 history entries exist.
    pub sharpe_ratio: f64,

    /// Maximum peak-to-trough drawdown observed in the history window,
    /// expressed as a fraction of peak equity (e.g. `0.05` = 5% drawdown).
    pub max_drawdown: f64,

    /// Most recent peak-to-current drawdown (0.0 when at or above peak).
    pub current_drawdown: f64,

    /// Fraction of ticks where equity improved (wins / ticks).
    pub win_rate: f64,

    /// Mean per-tick change in total PnL over the history window.
    pub avg_tick_pnl: f64,

    /// Standard deviation of per-tick PnL changes.
    pub volatility: f64,

    pub computed_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// ExportFormat
// ---------------------------------------------------------------------------

/// Output format for `PerformanceAnalytics::export_metrics`.
///
/// All variants produce a `String` that callers can write to a file or log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// JSON array of all snapshots in the history window.
    Json,
    /// CSV with a header row followed by one row per snapshot.
    Csv,
    /// Prometheus text exposition format (latest snapshot only).
    Prometheus,
}
