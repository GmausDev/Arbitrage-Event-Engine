// crates/scenario_engine/src/types.rs

use std::collections::HashMap;

use chrono::{DateTime, Utc};

/// Canonical market identifier (opaque string, e.g. Polymarket condition ID).
pub type MarketId = String;

// ---------------------------------------------------------------------------
// Scenario types
// ---------------------------------------------------------------------------

/// One sampled future world state — a joint assignment of binary outcomes
/// across all tracked markets.
#[derive(Debug, Clone)]
pub struct Scenario {
    /// Index within the parent batch (0-based).
    pub scenario_id: u64,
    /// Sampled binary outcome per market.
    pub market_outcomes: HashMap<MarketId, bool>,
    /// Uniform weight = 1 / sample_size for unweighted Monte Carlo.
    pub probability_weight: f64,
    pub timestamp: DateTime<Utc>,
}

/// A collection of sampled worlds produced by one Monte Carlo pass.
#[derive(Debug, Clone)]
pub struct ScenarioBatch {
    pub scenarios: Vec<Scenario>,
    pub sample_size: usize,
    /// Wall-clock milliseconds to generate this batch.
    pub generation_time_ms: u64,
}

// ---------------------------------------------------------------------------
// Expectation / analysis types
// ---------------------------------------------------------------------------

/// Computed marginal and joint probabilities derived from a `ScenarioBatch`.
#[derive(Debug, Clone, Default)]
pub struct ScenarioExpectation {
    /// P(market = true) for each market, estimated from scenario frequencies.
    pub expected_probabilities: HashMap<MarketId, f64>,
    /// P(A = true ∧ B = true) for market pairs (A, B).
    /// Both orientations are stored: (A,B) and (B,A) have equal values.
    pub joint_probabilities: HashMap<(MarketId, MarketId), f64>,
}

// ---------------------------------------------------------------------------
// Internal engine state
// ---------------------------------------------------------------------------

/// A directed influence edge between two markets, inferred from `GraphUpdate`
/// events.  Used during sampling to apply first-order dependency adjustments.
#[derive(Debug, Clone)]
pub struct DependencyEdge {
    /// The market whose outcome influences `child`.
    pub parent: MarketId,
    /// The influenced market.
    pub child: MarketId,
    /// Signed influence weight in [-1, 1].
    /// Positive: parent=true pushes child probability up.
    /// Negative: parent=true pushes child probability down.
    pub weight: f64,
}

/// Mutable state maintained by the Scenario Engine across events.
#[derive(Debug, Clone, Default)]
pub struct ScenarioEngineState {
    /// Current world-model belief per market (from `WorldProbabilityUpdate`).
    pub world_probabilities: HashMap<MarketId, f64>,
    /// Evidence-density confidence per market (from `WorldSignal`).
    pub confidence_map: HashMap<MarketId, f64>,
    /// Raw market prices (from `MarketUpdate`), used for mispricing detection.
    pub market_prices: HashMap<MarketId, f64>,
    /// Learned dependency edges from `GraphUpdate` events.
    pub dependencies: Vec<DependencyEdge>,
    /// Incremented on every meaningful update; used to avoid redundant batches.
    pub generation: u64,
}

// ---------------------------------------------------------------------------
// Internal publishing helper
// ---------------------------------------------------------------------------

/// A mispricing signal assembled inside the analysis step, before publishing.
#[derive(Debug)]
pub struct PendingSignal {
    pub market_id: String,
    pub expected_probability: f64,
    pub market_probability: f64,
    pub mispricing: f64,
    pub confidence: f64,
}
