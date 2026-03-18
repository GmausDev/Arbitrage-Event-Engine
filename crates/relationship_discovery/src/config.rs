// crates/relationship_discovery/src/config.rs

use serde::{Deserialize, Serialize};

/// Configuration for `RelationshipDiscoveryEngine`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationshipDiscoveryConfig {
    // ── History ───────────────────────────────────────────────────────────────
    /// Maximum price samples retained per market.
    pub max_history_len: usize,
    /// Minimum paired observations required before computing pairwise statistics.
    pub min_history_len: usize,

    // ── Scoring weights (must sum to 1.0) ─────────────────────────────────────
    /// Weight of |correlation| in the composite score.
    pub correlation_weight: f64,
    /// Weight of normalised mutual information in the composite score.
    pub mutual_info_weight: f64,
    /// Weight of embedding cosine similarity in the composite score.
    pub embedding_weight: f64,

    // ── Emission thresholds ───────────────────────────────────────────────────
    /// Minimum composite score to emit a `RelationshipDiscovered` event.
    pub min_relationship_score: f64,
    /// Conditional-probability threshold for declaring an edge Directed.
    /// Both P(A_up|B_up) and P(A_down|B_down) must exceed this value.
    pub directed_threshold: f64,

    // ── Deduplication ─────────────────────────────────────────────────────────
    /// Seconds to suppress re-emitting the same market pair.
    pub edge_ttl_secs: u64,

    // ── Noise gate ────────────────────────────────────────────────────────────
    /// Minimum absolute probability change to record a new price sample.
    pub min_prob_change: f64,

    // ── MI / conditional binning ──────────────────────────────────────────────
    /// Delta magnitude above which a move is classified UP or DOWN.
    pub price_change_threshold: f64,

    // ── State eviction ────────────────────────────────────────────────────────
    /// Seconds since `last_seen` after which a market's history is evicted.
    /// Setting this to `u64::MAX` effectively disables market eviction.
    pub market_ttl_secs: u64,
    /// Number of market-update events (past warm-up) between eviction passes.
    /// Lower values reduce peak memory at the cost of more frequent scans.
    pub eviction_interval: u64,
}

impl Default for RelationshipDiscoveryConfig {
    fn default() -> Self {
        Self {
            max_history_len:       200,
            min_history_len:       20,
            correlation_weight:    0.40,
            mutual_info_weight:    0.30,
            embedding_weight:      0.30,
            min_relationship_score: 0.40,
            directed_threshold:    0.65,
            edge_ttl_secs:         3_600,
            min_prob_change:       0.001,
            price_change_threshold: 0.01,
            market_ttl_secs:       7_200,   // 2 hours
            eviction_interval:     1_000,   // scan every ~1 k events
        }
    }
}

impl RelationshipDiscoveryConfig {
    pub fn validate(&self) -> Result<(), String> {
        let w = self.correlation_weight + self.mutual_info_weight + self.embedding_weight;
        if (w - 1.0).abs() > 1e-9 {
            return Err(format!(
                "scoring weights must sum to 1.0, got {w:.6} \
                 (correlation={}, mi={}, embedding={})",
                self.correlation_weight, self.mutual_info_weight, self.embedding_weight
            ));
        }
        if self.max_history_len < self.min_history_len {
            return Err(format!(
                "max_history_len ({}) < min_history_len ({})",
                self.max_history_len, self.min_history_len
            ));
        }
        if self.min_history_len < 2 {
            return Err("min_history_len must be ≥ 2".into());
        }
        if !(0.0..=1.0).contains(&self.min_relationship_score) {
            return Err("min_relationship_score must be in [0, 1]".into());
        }
        if !(0.0..=1.0).contains(&self.directed_threshold) {
            return Err("directed_threshold must be in [0, 1]".into());
        }
        if self.max_history_len == 0 {
            return Err("max_history_len must be > 0".into());
        }
        if self.min_prob_change < 0.0 {
            return Err("min_prob_change must be >= 0.0".into());
        }
        if self.price_change_threshold < 0.0 {
            return Err("price_change_threshold must be >= 0.0".into());
        }
        if self.correlation_weight < 0.0
            || self.mutual_info_weight < 0.0
            || self.embedding_weight < 0.0
        {
            return Err("all scoring weights must be non-negative".into());
        }
        if self.eviction_interval == 0 {
            return Err("eviction_interval must be > 0".into());
        }
        Ok(())
    }
}
