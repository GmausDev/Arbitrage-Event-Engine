// crates/risk_engine/src/state.rs

use std::collections::HashMap;
use std::sync::Arc;

use common::TradeDirection;
use tokio::sync::RwLock;

/// A single open position tracked by the Risk Engine.
#[derive(Debug, Clone)]
pub struct Position {
    pub market_id:     String,
    pub size_fraction: f64,
    pub entry_price:   f64,
    pub direction:     TradeDirection,
}

/// Mutable runtime state of the Risk Engine.
#[derive(Debug)]
pub struct RiskState {
    /// Current bankroll in USD (used as reference for equity calculations).
    pub bankroll: f64,

    /// Sum of all currently approved position fractions.
    pub total_exposure: f64,

    /// Open positions keyed by market_id.
    pub positions: HashMap<String, Position>,

    /// Aggregate exposure per cluster, keyed by cluster_id prefix.
    pub cluster_exposure: HashMap<String, f64>,

    /// Highest equity watermark seen so far.
    pub peak_equity: f64,

    /// Current equity estimate (updated by portfolio events, defaults to bankroll).
    pub current_equity: f64,

    /// Total number of `TradeSignal` events received.
    pub signals_seen: u64,

    /// Signals that passed all checks and were forwarded as `ApprovedTrade`.
    pub trades_approved: u64,

    /// Signals that failed at least one check.
    pub trades_rejected: u64,
}

impl Default for RiskState {
    fn default() -> Self {
        Self {
            bankroll:         10_000.0,
            total_exposure:   0.0,
            positions:        HashMap::new(),
            cluster_exposure: HashMap::new(),
            peak_equity:      10_000.0,
            current_equity:   10_000.0,
            signals_seen:     0,
            trades_approved:  0,
            trades_rejected:  0,
        }
    }
}

pub type SharedRiskState = Arc<RwLock<RiskState>>;

/// Create a fresh shared state with default bankroll.
pub fn new_shared_state() -> SharedRiskState {
    Arc::new(RwLock::new(RiskState::default()))
}
