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

    // ── Cost model state ──────────────────────────────────────────────────────

    /// Per-market liquidity in USD, updated from `Event::Market` events.
    pub market_liquidity: HashMap<String, f64>,

    /// EMA of observed execution latency in ms, updated from `EdgeDecayReport`.
    /// Initialised to `CostModelConfig::default().expected_latency_ms`.
    pub avg_latency_ms: f64,

    /// Cumulative sum of gross edges seen (for edge-efficiency diagnostics).
    pub gross_edge_sum: f64,

    /// Cumulative sum of net edges after costs.
    pub net_edge_sum: f64,

    /// Cumulative sum of total costs.
    pub total_cost_sum: f64,

    /// Number of signals that reached the cost gate (denominator for averages).
    pub cost_trades_count: u64,

    /// Signals rejected because `net_edge ≤ 0`.
    pub trades_rejected_negative_edge: u64,

    /// Signals rejected because `expected_profit < min_expected_profit_usd`.
    pub trades_rejected_insufficient_profit: u64,
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
            market_liquidity:                    HashMap::new(),
            avg_latency_ms:                      50.0,
            gross_edge_sum:                      0.0,
            net_edge_sum:                        0.0,
            total_cost_sum:                      0.0,
            cost_trades_count:                   0,
            trades_rejected_negative_edge:       0,
            trades_rejected_insufficient_profit: 0,
        }
    }
}

pub type SharedRiskState = Arc<RwLock<RiskState>>;

/// Create a fresh shared state with default bankroll.
pub fn new_shared_state() -> SharedRiskState {
    Arc::new(RwLock::new(RiskState::default()))
}
