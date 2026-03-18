// crates/portfolio_engine/src/state.rs

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::RwLock;

use crate::types::PortfolioPosition;

/// Complete mutable state of the `PortfolioEngine`.
#[derive(Debug)]
pub struct PortfolioState {
    /// Active open positions indexed by `market_id`.
    pub positions: HashMap<String, PortfolioPosition>,

    /// Available cash in USD.
    ///
    /// Starts at `initial_bankroll`.  Decreases when a fill deploys capital;
    /// increases when a position is replaced (old capital is "returned") and
    /// when `apply_risk_adjustments` trims an oversized position.
    pub cash: f64,

    /// Sum of realised PnL across **all** positions, including those already
    /// closed (replaced by a new fill in the same market).
    pub total_realized_pnl: f64,

    /// Sum of unrealised PnL across all **currently open** positions.
    ///
    /// Recomputed by `recalculate_unrealized`; zero until the first price
    /// update arrives.
    pub total_unrealized_pnl: f64,

    /// Bankroll at engine construction.  Used as the denominator for exposure
    /// fractions and position-size caps.  Never changes after construction.
    pub initial_bankroll: f64,

    /// Wall-clock timestamp of the last state mutation.
    pub last_update: DateTime<Utc>,
}

impl PortfolioState {
    /// Create a fresh state with `initial_bankroll` cash and no open positions.
    pub fn new(initial_bankroll: f64) -> Self {
        Self {
            positions:            HashMap::new(),
            cash:                 initial_bankroll,
            total_realized_pnl:   0.0,
            total_unrealized_pnl: 0.0,
            initial_bankroll,
            last_update:          Utc::now(),
        }
    }

    /// Total equity: `initial_bankroll + total_realized_pnl + total_unrealized_pnl`.
    pub fn total_equity(&self) -> f64 {
        self.initial_bankroll + self.total_realized_pnl + self.total_unrealized_pnl
    }

    /// Total deployed capital in USD (sum of all open position sizes).
    pub fn total_deployed(&self) -> f64 {
        self.positions.values().map(|p| p.size).sum()
    }

    /// Exposure as a fraction of `initial_bankroll`.
    ///
    /// Returns 0.0 when `initial_bankroll` is zero (guards against division).
    pub fn exposure_fraction(&self) -> f64 {
        if self.initial_bankroll > 0.0 {
            self.total_deployed() / self.initial_bankroll
        } else {
            0.0
        }
    }
}

/// Thread-safe handle to `PortfolioState` shared across tasks.
pub type SharedPortfolioState = Arc<RwLock<PortfolioState>>;
