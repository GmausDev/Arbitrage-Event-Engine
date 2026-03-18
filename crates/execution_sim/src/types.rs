// crates/execution_sim/src/types.rs

use chrono::{DateTime, Utc};
use common::TradeDirection;

/// An execution order derived from an `ApprovedTrade`.
///
/// Created internally by the `ExecutionSimulator` before being passed through
/// the slippage / partial-fill simulation logic.
#[derive(Debug, Clone)]
pub struct ExecutionOrder {
    pub market_id:    String,
    pub direction:    TradeDirection,
    /// Requested size as a fraction of bankroll (= `approved_fraction`).
    pub quantity:     f64,
    /// Market mid-price at submission time (= `market_prob`, in `[0, 1]`).
    pub price:        f64,
    /// Maximum acceptable slippage as a fraction of `price`.
    ///
    /// Orders where simulated slippage exceeds this cap are downgraded to
    /// a partial fill with quantity reduced proportionally.
    pub max_slippage: f64,
    pub timestamp:    DateTime<Utc>,
}
