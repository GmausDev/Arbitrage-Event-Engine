// crates/common/src/types.rs
// Core domain types shared across all crates.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Millisecond-precision UTC timestamp alias.
pub type Timestamp = DateTime<Utc>;

// ---------------------------------------------------------------------------
// Market primitives
// ---------------------------------------------------------------------------

/// A single market node in the market graph.
/// Represents the current view of one prediction market.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketNode {
    /// Unique market identifier (e.g. Polymarket condition ID)
    pub id: String,
    /// Current implied probability [0.0, 1.0]
    pub probability: f64,
    /// Available liquidity in USD
    pub liquidity: f64,
    /// Time of last update
    pub last_update: Timestamp,
}

/// An edge between two market nodes expressing their correlation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketEdge {
    /// Pearson correlation coefficient [-1.0, 1.0]
    pub correlation: f64,
}

// ---------------------------------------------------------------------------
// Trade signal
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum TradeDirection {
    Buy,
    Sell,
    /// Cross-market arbitrage
    Arbitrage,
}

/// Fast/slow classification assigned by `SignalPriorityEngine`.
///
/// * `Fast` — high-edge signal; bypasses the `PortfolioOptimizer` batch window
///   and routes directly to `RiskEngine` via `Event::FastSignal`.
/// * `Slow` — lower-edge signal; buffered in the priority queue and emitted as
///   part of a `TopSignalsBatch` for portfolio-aware allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SignalSpeed {
    Fast,
    Slow,
}

/// A `TradeSignal` decorated with a priority score and speed classification,
/// emitted inside `TopSignalsBatch` events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrioritizedSignal {
    pub signal:         TradeSignal,
    /// `expected_value × confidence` — ranking key used by `SignalPriorityEngine`.
    pub priority_score: f64,
    pub speed:          SignalSpeed,
}

/// Signal emitted by signal_agent; consumed by risk_engine and execution_sim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TradeSignal {
    pub market_id: String,
    pub direction: TradeDirection,
    /// Expected value after subtracting trading costs: posterior − market − cost (BUY)
    /// or market − posterior − cost (SELL).
    pub expected_value: f64,
    /// Kelly-sized position fraction (fractional Kelly, clamped to max_position_fraction).
    pub position_fraction: f64,
    /// Bayesian posterior probability for this market.
    pub posterior_prob: f64,
    /// Current market-implied probability at signal time.
    pub market_prob: f64,
    /// Model confidence in [0, 1] — evidence density sqrt(history_len / MAX_HISTORY).
    pub confidence: f64,
    pub timestamp: Timestamp,
    /// Originating strategy agent (e.g. "signal_agent", "graph_arb_agent").
    /// Empty string when the source is unknown or a legacy stub.
    #[serde(default)]
    pub source: String,
}

// ---------------------------------------------------------------------------
// Portfolio
// ---------------------------------------------------------------------------

/// Current open position in a market.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub market_id: String,
    pub direction: TradeDirection,
    pub size: f64,
    pub entry_probability: f64,
    pub opened_at: Timestamp,
}

/// Aggregate portfolio state snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Portfolio {
    pub positions: Vec<Position>,
    /// Realised + unrealised PnL in USD
    pub pnl: f64,
    /// Total deployed capital in USD
    pub exposure: f64,
}
