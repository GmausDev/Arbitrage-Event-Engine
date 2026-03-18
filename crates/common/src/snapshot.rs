// crates/common/src/snapshot.rs
// Immutable market-state snapshot passed to engines and agents each tick.

use crate::types::{MarketNode, Portfolio, Timestamp};
use std::collections::HashMap;
use std::sync::Arc;

/// Immutable view of the full market state at a single point in time.
///
/// Wrapped in `Arc` so it can be cheaply cloned and shared across tasks
/// without copying the underlying data.
///
/// All engines receive the same `Arc<Snapshot>` per tick; none may mutate it.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// All known markets keyed by their market ID
    pub markets: HashMap<String, MarketNode>,
    /// Current portfolio state
    pub portfolio: Portfolio,
    /// Wall-clock time this snapshot was created
    pub timestamp: Timestamp,
}

impl Snapshot {
    pub fn new(
        markets: HashMap<String, MarketNode>,
        portfolio: Portfolio,
        timestamp: Timestamp,
    ) -> Arc<Self> {
        Arc::new(Self {
            markets,
            portfolio,
            timestamp,
        })
    }

    /// Convenience: look up a market by ID.
    pub fn get_market(&self, id: &str) -> Option<&MarketNode> {
        self.markets.get(id)
    }
}
