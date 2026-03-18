// crates/relationship_discovery/src/types.rs
//
// Internal data structures for the Relationship Discovery Engine.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Market pair key
// ---------------------------------------------------------------------------

/// Canonical market pair — (a, b) where a ≤ b lexicographically.
pub type MarketPair = (String, String);

/// Returns the canonical pair for two market IDs.
#[inline]
pub fn canonical_pair(a: &str, b: &str) -> MarketPair {
    if a <= b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

// ---------------------------------------------------------------------------
// Discrete price-move state (for MI and conditional probability)
// ---------------------------------------------------------------------------

/// Classification of a probability delta into a discrete move direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MoveState {
    Up,
    Down,
    Flat,
}

/// Classify a probability delta into a `MoveState`.
#[inline]
pub(crate) fn classify(delta: f64, threshold: f64) -> MoveState {
    if delta > threshold {
        MoveState::Up
    } else if delta < -threshold {
        MoveState::Down
    } else {
        MoveState::Flat
    }
}

// ---------------------------------------------------------------------------
// Joint state-transition counts for a market pair
// ---------------------------------------------------------------------------

/// Co-movement state counts for a canonical pair (a, b).
/// Keys: (state_of_a, state_of_b) on each co-observed price step.
#[derive(Debug, Clone, Default)]
pub struct JointCounts {
    pub counts: HashMap<(MoveState, MoveState), u64>,
}

// ---------------------------------------------------------------------------
// Per-pair joint history
// ---------------------------------------------------------------------------

/// Paired probability observations for a canonical market pair (a, b).
#[derive(Debug, Default)]
pub struct JointEntry {
    /// Rolling window of (prob_a, prob_b, move_classification) samples.
    ///
    /// The third element is `Some((sa, sb))` for all entries except the very
    /// first, which has no predecessor to diff against.  It is stored so that
    /// the count contributed to `joint_counts` can be decremented when the
    /// entry is evicted, keeping `joint_counts` a true rolling-window summary.
    pub pairs: VecDeque<(f64, f64, Option<(MoveState, MoveState)>)>,
    /// Rolling-window joint move-state counts (kept in sync with `pairs`).
    pub joint_counts: JointCounts,
    /// Timestamp of the most recent `RelationshipDiscovered` emission
    /// (used for deduplication / TTL).
    pub last_emitted_at: Option<DateTime<Utc>>,
}

impl JointEntry {
    /// Push a new paired observation, updating the rolling window and
    /// joint move-state counts.  When the window is full, the oldest entry
    /// is evicted and its count contribution is decremented so that
    /// `joint_counts` always reflects only the current window.
    pub fn push(&mut self, pa: f64, pb: f64, window: usize, threshold: f64) {
        // Compute move states from the most recent sample (if any) and
        // increment the joint count for this transition.
        let classification = if let Some(&(prev_a, prev_b, _)) = self.pairs.back() {
            let sa = classify(pa - prev_a, threshold);
            let sb = classify(pb - prev_b, threshold);
            *self.joint_counts.counts.entry((sa, sb)).or_default() += 1;
            Some((sa, sb))
        } else {
            None // first sample has no predecessor
        };

        self.pairs.push_back((pa, pb, classification));

        // Evict the oldest entry and decrement its count contribution.
        if self.pairs.len() > window {
            if let Some((_, _, Some((sa, sb)))) = self.pairs.pop_front() {
                if let Some(c) = self.joint_counts.counts.get_mut(&(sa, sb)) {
                    *c = c.saturating_sub(1);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Per-market rolling history
// ---------------------------------------------------------------------------

/// Rolling probability history for a single market.
#[derive(Debug, Default)]
pub struct MarketHistory {
    /// Recent probability samples (most recent last).
    pub prices: VecDeque<f64>,
    /// Cached TF-IDF token embedding derived from the market ID.
    /// Computed once on the first market update and reused thereafter.
    pub embedding: Option<Vec<(String, f64)>>,
    pub last_seen: DateTime<Utc>,
    /// Set when a new price passes the noise gate; cleared when this
    /// market's price is captured in a joint-pair entry with a peer.
    /// Joint entries are only pushed when *both* markets are dirty,
    /// ensuring observations reflect genuine simultaneous co-movement.
    pub dirty: bool,
}

// ---------------------------------------------------------------------------
// Engine state
// ---------------------------------------------------------------------------

/// Shared mutable state for `RelationshipDiscoveryEngine`.
#[derive(Debug, Default)]
pub struct RelationshipDiscoveryState {
    /// Rolling price history per market.
    pub histories: HashMap<String, MarketHistory>,
    /// Per-pair joint observations and move-state counts.
    pub joint_entries: HashMap<MarketPair, JointEntry>,
    /// Total `Event::Market` events processed (including noise-filtered ones).
    pub events_processed: u64,
    /// Total `RelationshipDiscovered` events emitted.
    pub relationships_discovered: u64,
    /// Events processed since the last eviction pass (resets to 0 each pass).
    pub events_since_eviction: u64,
}

pub type SharedRelationshipDiscoveryState = Arc<RwLock<RelationshipDiscoveryState>>;

pub fn new_shared_state() -> SharedRelationshipDiscoveryState {
    Arc::new(RwLock::new(RelationshipDiscoveryState::default()))
}
