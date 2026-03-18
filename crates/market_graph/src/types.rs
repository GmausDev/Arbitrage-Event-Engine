// crates/market_graph/src/types.rs
// Rich domain types used internally by the Market Graph Engine.
// These are separate from the wire-format types in `common` — the async
// wrapper in lib.rs translates between them via `From<common::MarketNode>`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── Update source ─────────────────────────────────────────────────────────────

/// Who triggered a probability update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateSource {
    /// Live price from Polymarket / Kalshi API
    Api,
    /// Manually entered by the operator
    Manual,
    /// Derived from a news / sentiment event
    News,
    /// Internal graph propagation (written by the engine itself)
    Propagation,
    /// Backtesting or simulation replay
    Simulation,
}

// ── Edge type ─────────────────────────────────────────────────────────────────

/// Classification of why two markets are related.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeType {
    /// Direct causal link (e.g. "Trump wins presidency" → "GOP controls Senate")
    Causal,
    /// Correlation measured from historical price data
    Statistical,
    /// Topic / keyword overlap detected by the NLP layer
    Semantic,
    /// Asserted manually by an analyst
    Manual,
}

// ── Market node ───────────────────────────────────────────────────────────────

/// A single prediction market represented as a graph node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketNode {
    /// Unique market identifier (Polymarket condition ID, Kalshi ticker, etc.)
    pub market_id: String,

    /// Human-readable title
    pub title: String,

    /// Current market-reported probability [0.0, 1.0]
    pub current_prob: f64,

    /// Traded volume / available liquidity in USD
    pub volume: Option<f64>,

    /// Resolution date (ISO 8601)
    pub resolution_date: Option<String>,

    /// Our net position in shares
    /// Positive = long YES, negative = long NO (short YES)
    pub our_position: f64,

    /// Graph-implied probability after belief propagation [0.0, 1.0]
    ///
    /// Initialised to `current_prob` on insertion.
    /// Updated by `MarketGraphEngine::propagate()`.
    pub implied_prob: f64,

    /// Source of the most recent `current_prob` update
    pub last_source: UpdateSource,

    /// Wall-clock timestamp of the most recent update
    pub last_update: DateTime<Utc>,

    /// Niche / topic tags for filtering and clustering
    /// e.g. `["politics", "us-election-2028"]`
    pub tags: Vec<String>,
}

impl MarketNode {
    /// Construct a new node. `implied_prob` is initialised to `current_prob`.
    pub fn new(
        market_id: impl Into<String>,
        title: impl Into<String>,
        prob: f64,
        volume: Option<f64>,
        resolution_date: Option<String>,
    ) -> Self {
        let p = prob.clamp(0.0, 1.0);
        Self {
            market_id: market_id.into(),
            title: title.into(),
            current_prob: p,
            volume,
            resolution_date,
            our_position: 0.0,
            implied_prob: p,
            last_source: UpdateSource::Manual,
            last_update: Utc::now(),
            tags: Vec::new(),
        }
    }

    /// Builder: attach topic tags.
    pub fn with_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tags = tags.into_iter().map(Into::into).collect();
        self
    }

    /// Builder: set our current position in this market.
    pub fn with_position(mut self, shares: f64) -> Self {
        self.our_position = shares;
        self
    }

    /// Deviation: `current_prob - implied_prob`.
    ///
    /// * **Positive** → market is trading *above* the graph's estimate
    ///   (possible SELL / fade signal).
    /// * **Negative** → market is trading *below* the graph's estimate
    ///   (possible BUY signal).
    #[inline]
    #[must_use]
    pub fn deviation(&self) -> f64 {
        self.current_prob - self.implied_prob
    }
}

// ── Market edge ───────────────────────────────────────────────────────────────

/// A directed weighted dependency between two markets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketEdge {
    /// Source market ID (from)
    pub from_id: String,

    /// Target market ID (to)
    pub to_id: String,

    /// Correlation strength [-1.0, 1.0]
    ///
    /// * Positive → the two markets tend to move in the **same** direction.
    /// * Negative → they tend to move in **opposite** directions.
    pub weight: f64,

    /// Confidence in this relationship [0.0, 1.0]
    /// Applied as a multiplier: `effective_weight = weight × confidence × decay`
    pub confidence: f64,

    /// How this edge was established
    pub edge_type: EdgeType,

    /// Timestamp when the edge was created (used for time decay)
    pub created_at: DateTime<Utc>,

    /// Optional exponential-decay half-life in days.
    ///
    /// If set, confidence decays as `e^(−t × ln2 / half_life)` where `t` is
    /// the age of the edge in days. Useful for edges derived from short-lived
    /// news or statistical windows.
    pub decay_half_life_days: Option<f64>,
}

impl MarketEdge {
    pub fn new(
        from_id: impl Into<String>,
        to_id: impl Into<String>,
        weight: f64,
        confidence: f64,
        edge_type: EdgeType,
    ) -> Self {
        Self {
            from_id: from_id.into(),
            to_id: to_id.into(),
            weight: weight.clamp(-1.0, 1.0),
            confidence: confidence.clamp(0.0, 1.0),
            edge_type,
            created_at: Utc::now(),
            decay_half_life_days: None,
        }
    }

    /// Builder: attach a time-decay half-life.
    ///
    /// # Panics (debug builds)
    /// Panics if `half_life_days` is not a positive finite number.
    pub fn with_decay(mut self, half_life_days: f64) -> Self {
        debug_assert!(
            half_life_days.is_finite() && half_life_days > 0.0,
            "decay half-life must be a positive finite number, got {half_life_days}"
        );
        self.decay_half_life_days = Some(half_life_days.max(f64::EPSILON));
        self
    }

    /// Effective transmission strength at `now` = `weight × confidence × time_decay`.
    ///
    /// Prefer this in hot loops — capture `now` once with `Utc::now()` before
    /// the loop and pass it here to avoid repeated syscalls.
    #[must_use]
    pub fn effective_weight_at(&self, now: DateTime<Utc>) -> f64 {
        let decay = match self.decay_half_life_days {
            Some(half_life) if half_life > 0.0 => {
                let age_days =
                    (now - self.created_at).num_seconds() as f64 / 86_400.0;
                // λ = ln(2) / half_life  →  decay = e^(−λ × age)
                (-age_days * std::f64::consts::LN_2 / half_life).exp()
            }
            _ => 1.0,
        };
        self.weight * self.confidence * decay
    }

    /// Effective transmission strength at the current wall-clock time.
    ///
    /// Convenience wrapper; calls `Utc::now()` on every invocation.
    /// Use [`effective_weight_at`](Self::effective_weight_at) in tight loops.
    #[must_use]
    pub fn effective_weight(&self) -> f64 {
        self.effective_weight_at(Utc::now())
    }
}

// ── Propagation configuration ─────────────────────────────────────────────────

/// Tuning parameters for the BFS belief-propagation algorithm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropagationConfig {
    /// Minimum `|current_prob − implied_prob|` required to start a propagation
    /// pass (avoids wasting CPU on noise). Default: 0.005 (0.5 %).
    pub change_threshold: f64,

    /// Minimum implied-probability change to continue propagating downstream.
    /// Acts as a per-edge convergence criterion. Default: 0.001 (0.1 %).
    pub convergence_threshold: f64,

    /// Multiplicative damping applied to every message at each hop.
    ///
    /// Prevents oscillation in graphs with cycles. A value of 0.85 means a
    /// signal loses 15 % of its strength each time it crosses an edge.
    /// Default: 0.85.
    pub damping_factor: f64,

    /// Maximum BFS hops from the updated market before propagation stops.
    ///
    /// This is the primary depth limiter: a task at depth ≥ `max_depth` is
    /// dropped from the queue without processing.  Combined with
    /// `convergence_threshold`, this bounds both the breadth and depth of each
    /// propagation pass and prevents long chain-reactions in large graphs.
    /// Default: 3.
    pub max_depth: usize,
}

impl Default for PropagationConfig {
    fn default() -> Self {
        Self {
            change_threshold: 0.005,
            convergence_threshold: 0.001,
            damping_factor: 0.85,
            max_depth: 3,
        }
    }
}

// ── Portable snapshot ─────────────────────────────────────────────────────────

/// A serialisable snapshot of the entire graph, used for persistence and
/// cross-process transfer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphSnapshot {
    pub nodes: Vec<MarketNode>,
    pub edges: Vec<MarketEdge>,
    pub config: PropagationConfig,
}

// ── Propagation result ────────────────────────────────────────────────────────

/// The `implied_prob` change for a single node produced by one propagation pass.
#[derive(Debug, Clone)]
pub struct NodeChange {
    /// The market whose `implied_prob` changed.
    pub market_id: String,
    /// `implied_prob` before this propagation pass (first time this node was
    /// touched; preserved across multiple messages from different upstream nodes).
    pub old_implied_prob: f64,
    /// `implied_prob` after all contributions have been applied.
    pub new_implied_prob: f64,
}

/// Summary statistics returned by `MarketGraphEngine::propagate()`.
#[derive(Debug, Clone)]
pub struct PropagationResult {
    /// Number of **distinct** nodes whose `implied_prob` changed this pass.
    pub nodes_updated: usize,
    /// Number of BFS queue entries processed (does not count depth-skipped entries).
    pub iterations: usize,
    /// `true` when no tasks were dropped due to `max_depth`; `false` when
    /// the depth limit cut off further propagation.
    pub converged: bool,
    /// Market IDs of all nodes updated (convenience view over `node_changes`).
    pub updated_node_ids: Vec<String>,
    /// Full before/after records for every updated node.
    ///
    /// Use these to populate `GraphUpdate.node_updates` without a separate
    /// graph read — the old and new `implied_prob` values are recorded here.
    pub node_changes: Vec<NodeChange>,
}
