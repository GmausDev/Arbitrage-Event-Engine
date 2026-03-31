// crates/persistence/src/models.rs
//
// Data models for persisted records.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A completed trade record with full P&L attribution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeRecord {
    /// Auto-increment ID assigned by the database.
    pub id: Option<i64>,
    /// Exchange the order was placed on ("polymarket", "kalshi", "sim").
    pub exchange: String,
    /// Exchange-assigned order ID (if real), or OMS-generated ID (if sim).
    pub order_id: String,
    /// Market/ticker identifier.
    pub market_id: String,
    /// "buy_yes" | "buy_no"
    pub side: String,
    /// Originating strategy agent.
    pub signal_source: String,

    // ── Sizing ───────────────────────────────────────────────────────────────
    /// Requested order size in USD.
    pub requested_size_usd: f64,
    /// Actually filled size in USD.
    pub filled_size_usd: f64,
    /// Fill ratio [0.0, 1.0].
    pub fill_ratio: f64,

    // ── Pricing ──────────────────────────────────────────────────────────────
    /// Market mid-price at signal time.
    pub market_price: f64,
    /// Model posterior probability at signal time.
    pub posterior_prob: f64,
    /// Volume-weighted average fill price.
    pub avg_fill_price: f64,

    // ── P&L attribution ──────────────────────────────────────────────────────
    /// Gross edge: |posterior - market_price|
    pub gross_edge: f64,
    /// Estimated fee cost (from cost model).
    pub fee_cost: f64,
    /// Estimated spread cost.
    pub spread_cost: f64,
    /// Estimated slippage cost.
    pub slippage_cost: f64,
    /// Estimated latency decay cost.
    pub decay_cost: f64,
    /// Total estimated cost (fees + spread + slippage + decay).
    pub total_cost: f64,
    /// Net edge after costs.
    pub net_edge: f64,
    /// Actual slippage: (avg_fill_price - market_price) / market_price.
    pub realized_slippage: f64,
    /// Expected profit at time of approval.
    pub expected_profit_usd: f64,

    // ── Lifecycle ────────────────────────────────────────────────────────────
    /// Final order status.
    pub status: String,
    /// When the original signal was generated.
    pub signal_timestamp: DateTime<Utc>,
    /// When the order was submitted.
    pub submitted_at: DateTime<Utc>,
    /// When the fill was recorded.
    pub filled_at: DateTime<Utc>,
    /// End-to-end latency in milliseconds (signal → fill).
    pub latency_ms: i64,
}

/// Equity snapshot at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquitySnapshot {
    pub id: Option<i64>,
    /// Total equity: cash + unrealized + realized.
    pub equity_usd: f64,
    /// Cash available.
    pub cash_usd: f64,
    /// Total deployed capital.
    pub deployed_usd: f64,
    /// Number of open positions.
    pub open_positions: i64,
    /// Cumulative realized P&L.
    pub realized_pnl: f64,
    /// Current unrealized P&L.
    pub unrealized_pnl: f64,
    /// Peak equity (high-water mark).
    pub peak_equity: f64,
    /// Current drawdown fraction from peak.
    pub drawdown: f64,
    pub timestamp: DateTime<Utc>,
}

/// Audit trail entry for significant system events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: Option<i64>,
    /// Event category: "trade", "rejection", "config_change", "reconciliation",
    /// "error", "startup", "shutdown".
    pub category: String,
    /// Human-readable summary.
    pub summary: String,
    /// Full event payload as JSON (for debugging).
    pub details_json: String,
    pub timestamp: DateTime<Utc>,
}

/// Result of a reconciliation check between internal and exchange state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationResult {
    pub exchange: String,
    /// Internal tracked balance.
    pub internal_balance: f64,
    /// Balance reported by exchange.
    pub exchange_balance: f64,
    /// Absolute difference.
    pub balance_diff: f64,
    /// Number of position mismatches.
    pub position_mismatches: usize,
    /// Details of each mismatch.
    pub mismatch_details: Vec<String>,
    pub timestamp: DateTime<Utc>,
}
