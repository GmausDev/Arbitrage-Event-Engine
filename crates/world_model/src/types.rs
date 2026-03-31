// crates/world_model/src/types.rs

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::WorldModelError;

/// Canonical identifier for a prediction market.
pub type MarketId = String;

// ---------------------------------------------------------------------------
// Core world-state types
// ---------------------------------------------------------------------------

/// The world model's belief about a single prediction market.
///
/// `current_probability` is the world-model's inferred estimate after global
/// belief propagation.  `market_probability` caches the last raw market price
/// observed via `Event::Market` or `Event::Posterior`, and is used to compute
/// the actionable edge emitted in `WorldSignal` events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketVariable {
    pub market_id: MarketId,
    /// World-model inferred probability (may differ from raw market price after
    /// propagation through dependent markets).
    pub current_probability: f64,
    /// Last known raw market price, if any.
    pub market_probability: Option<f64>,
    /// Evidence-density confidence [0, 1].
    pub confidence: f64,
    pub last_update_timestamp: DateTime<Utc>,
}

impl MarketVariable {
    pub fn new(market_id: impl Into<String>, probability: f64) -> Self {
        Self {
            market_id: market_id.into(),
            current_probability: probability,
            market_probability: None,
            confidence: 0.5,
            last_update_timestamp: Utc::now(),
        }
    }
}

/// A probabilistic dependency between two markets in the world model's own
/// dependency graph (distinct from the market-graph correlation graph).
///
/// When the parent market's probability changes by `Δ` (in log-odds), the
/// child receives an update of `weight × confidence × Δ`, attenuated by the
/// engine's `propagation_damping` per hop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldDependency {
    pub parent_market: MarketId,
    pub child_market: MarketId,
    /// Signed influence weight in [-1, 1].
    /// Positive = correlated (parent↑ → child↑).
    /// Negative = anti-correlated (parent↑ → child↓).
    pub weight: f64,
    /// Optional propagation delay in seconds (informational; not enforced
    /// in the async engine's current tick-based model).
    pub lag_seconds: f64,
    /// Confidence in this dependency [0, 1]; scales the propagated delta.
    pub confidence: f64,
}

// ---------------------------------------------------------------------------
// Constraint types
// ---------------------------------------------------------------------------

/// The logical relationship enforced by a [`LogicalConstraint`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ConstraintType {
    /// All listed markets are mutually exclusive outcomes; their probabilities
    /// must sum to 1.0 within `tolerance`.
    MutualExclusion,

    /// The last market in `markets_involved` represents "A AND B …".
    /// Its probability must not exceed `min(P(parents))`.
    And,

    /// The last market in `markets_involved` represents "A OR B …".
    /// Its probability must be ≥ `max(P(inputs))`.
    Or,

    /// A generalised sum-to-one constraint; equivalent to `MutualExclusion`
    /// but semantically distinct (e.g. probabilities of exhaustive events).
    SumToOne,
}

/// A logical rule that the world model enforces across a set of markets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogicalConstraint {
    pub constraint_id: String,
    pub constraint_type: ConstraintType,
    /// Markets subject to this constraint, in order.
    /// For `And` and `Or`, the *last* entry is the conjunction/disjunction result.
    pub markets_involved: Vec<MarketId>,
    /// Maximum allowed violation magnitude before emitting
    /// `Event::Inconsistency`.
    pub tolerance: f64,
}

// ---------------------------------------------------------------------------
// World state
// ---------------------------------------------------------------------------

/// The global probabilistic state maintained by [`WorldModelEngine`].
#[derive(Debug, Clone, Default)]
pub struct WorldState {
    /// Per-market belief variables.
    pub variables: HashMap<MarketId, MarketVariable>,
    /// Directed dependency edges used for belief propagation.
    pub dependencies: Vec<WorldDependency>,
    /// Logical constraints to check after every inference pass.
    pub constraints: Vec<LogicalConstraint>,
    /// Timestamp of the last completed inference pass, if any.
    pub last_inference_time: Option<DateTime<Utc>>,
}

impl WorldState {
    /// Add a dependency edge, replacing any existing edge between the same
    /// `(parent_market, child_market)` pair to prevent duplicate propagation.
    ///
    /// Returns the previous dependency if one was replaced.
    ///
    /// # Errors
    ///
    /// Returns `WorldModelError::InvalidWeight` if `dep.weight` is outside [-1.0, 1.0].
    /// Returns `WorldModelError::InvalidConfidence` if `dep.confidence` is outside [0.0, 1.0].
    pub fn add_dependency(
        &mut self,
        dep: WorldDependency,
    ) -> Result<Option<WorldDependency>, WorldModelError> {
        if !(-1.0..=1.0).contains(&dep.weight) {
            return Err(WorldModelError::InvalidWeight(dep.weight));
        }
        if !(0.0..=1.0).contains(&dep.confidence) {
            return Err(WorldModelError::InvalidConfidence(dep.confidence));
        }

        if let Some(pos) = self.dependencies.iter().position(|d| {
            d.parent_market == dep.parent_market && d.child_market == dep.child_market
        }) {
            Ok(Some(std::mem::replace(&mut self.dependencies[pos], dep)))
        } else {
            self.dependencies.push(dep);
            Ok(None)
        }
    }
}

// ---------------------------------------------------------------------------
// Inference result types
// ---------------------------------------------------------------------------

/// Describes one influence propagation edge traversed during BFS.
#[derive(Debug, Clone)]
pub struct InfluencePropagation {
    pub from_market: MarketId,
    pub to_market: MarketId,
    /// Log-odds delta applied to `to_market`.
    pub delta_log_odds: f64,
    /// Edge weight used for this propagation.
    pub weight: f64,
}

/// A violated logical constraint, with evidence for investigation.
#[derive(Debug, Clone)]
pub struct ConstraintViolation {
    pub constraint_id: String,
    pub constraint_type: ConstraintType,
    pub markets_involved: Vec<MarketId>,
    /// Current world-model probability of each involved market.
    pub current_probabilities: HashMap<MarketId, f64>,
    /// How far the constraint is violated (e.g. |sum − 1| for mutual exclusion).
    pub violation_magnitude: f64,
}

/// Output of one inference pass, collected for event publishing.
#[derive(Debug, Clone, Default)]
pub struct WorldInferenceResult {
    /// World-model probabilities after inference, keyed by market ID.
    /// Includes both the source market and all downstream markets updated.
    pub updated_probabilities: HashMap<MarketId, f64>,
    /// Constraint violations detected after this inference pass.
    pub detected_inconsistencies: Vec<ConstraintViolation>,
    /// Sequence of propagation edges traversed.
    pub influence_propagations: Vec<InfluencePropagation>,
}
