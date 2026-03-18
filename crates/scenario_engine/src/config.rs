// crates/scenario_engine/src/config.rs

use serde::{Deserialize, Serialize};

/// Configuration for the Scenario Engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioEngineConfig {
    /// Number of Monte Carlo scenarios to generate per batch.
    /// Higher values improve accuracy but cost more CPU.
    pub sample_size: usize,

    /// Minimum |expected_prob − market_prob| required to emit a `ScenarioSignal`.
    pub min_mispricing_threshold: f64,

    /// Maximum number of joint probability pairs to include in each
    /// `ScenarioExpectations` event.  Pairs are ranked by deviation from
    /// independence: |P(A∧B) − P(A)·P(B)|.  Set to 0 to include all pairs.
    pub max_joint_market_pairs: usize,

    /// Maximum number of dependency edges retained in state.
    /// When the limit is reached, the edge with the smallest absolute weight
    /// is evicted before inserting the new edge.  Set to 0 for no cap.
    pub max_dependencies: usize,
}

impl Default for ScenarioEngineConfig {
    fn default() -> Self {
        Self {
            sample_size: 1_000,
            min_mispricing_threshold: 0.05,
            max_joint_market_pairs: 50,
            max_dependencies: 500,
        }
    }
}

impl ScenarioEngineConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.sample_size == 0 {
            return Err("sample_size must be > 0".to_string());
        }
        // Use !(x >= 0.0) so NaN is also rejected (NaN >= 0.0 is false).
        if !(self.min_mispricing_threshold >= 0.0)
            || self.min_mispricing_threshold > 1.0
        {
            return Err(format!(
                "min_mispricing_threshold must be in [0, 1], got {}",
                self.min_mispricing_threshold
            ));
        }
        Ok(())
    }
}
