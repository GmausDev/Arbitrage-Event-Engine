// crates/meta_strategy/src/state.rs

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use common::TradeSignal;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Per-signal entry
// ---------------------------------------------------------------------------

/// One strategy agent's latest signal for a single market, with its arrival time.
#[derive(Debug, Clone)]
pub struct SignalEntry {
    pub signal: TradeSignal,
    /// Wall-clock time when this entry was ingested by the engine.
    pub received_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Per-market aggregation state
// ---------------------------------------------------------------------------

/// All signal entries for one market, keyed by the signal's `source` field.
///
/// Only the most recent signal per strategy is kept (newer arrivals overwrite
/// older ones).  The shock fields are updated by `Event::Shock`.
#[derive(Debug, Clone, Default)]
pub struct MarketMeta {
    /// Latest signal per strategy agent (`source` → `SignalEntry`).
    pub entries: HashMap<String, SignalEntry>,
    /// Magnitude of the most recent `InformationShock` for this market.
    pub shock_magnitude: f64,
    /// Timestamp of the most recent `InformationShock`, if any.
    pub shock_ts: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Engine-wide state
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct MetaStrategyState {
    /// Per-market aggregation state.
    pub markets: HashMap<String, MarketMeta>,
    /// Total number of events processed (incremented for every Signal/Shock).
    pub events_processed: u64,
    /// Total number of MetaSignals published.
    pub signals_emitted: u64,
}

/// Thread-safe handle to `MetaStrategyState`.
pub type SharedMetaStrategyState = Arc<RwLock<MetaStrategyState>>;

pub fn new_shared_state() -> SharedMetaStrategyState {
    Arc::new(RwLock::new(MetaStrategyState::default()))
}
