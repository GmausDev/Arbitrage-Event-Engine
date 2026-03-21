// crates/portfolio_engine/src/types.rs

use chrono::{DateTime, Utc};
use common::TradeDirection;

/// Rich per-position state maintained by `PortfolioEngine`.
///
/// Unlike `common::Position` (which is a thin wire type used for publishing),
/// this struct tracks the full lifecycle of a position: accumulated realised PnL,
/// current mark-to-market unrealised PnL, the last observed market price, and
/// optional strategy/niche tags.
#[derive(Debug, Clone)]
pub struct PortfolioPosition {
    /// Unique market identifier (matches `ApprovedTrade::market_id`).
    pub market_id: String,

    /// Trade direction for this position.
    pub direction: TradeDirection,

    /// Current absolute position size in USD.
    ///
    /// `size = executed_quantity × initial_bankroll`
    pub size: f64,

    /// Average executed entry price in probability units ([0, 1]).
    pub avg_price: f64,

    /// Accumulated realised PnL for this market (USD).
    ///
    /// Starts at 0.0 when the position is first opened.  When a position is
    /// replaced by a new fill, the realised gain/loss is crystallised here
    /// before the old position is removed.
    pub realized_pnl: f64,

    /// Current mark-to-market unrealised PnL (USD).
    ///
    /// Recomputed on every `recalculate_unrealized` call.
    pub unrealized_pnl: f64,

    /// Most recent market price seen for this market, if any.
    ///
    /// Seeded from `ApprovedTrade::market_prob` at fill time and updated by
    /// `recalculate_unrealized` as fresh `MarketUpdate` prices arrive.
    pub last_market_price: Option<f64>,

    /// When the current position was opened (or last replaced by a new fill).
    pub opened_at: DateTime<Utc>,

    /// Originating strategy agent (e.g. "signal_agent", "graph_arb_agent").
    /// Sourced from `ApprovedTrade::signal_source` at fill time.
    /// Used to bucket closed-position PnL by strategy for performance attribution.
    pub source: String,

    /// Optional metadata tags (e.g. niche, signal source).
    pub tags: Vec<String>,
}
