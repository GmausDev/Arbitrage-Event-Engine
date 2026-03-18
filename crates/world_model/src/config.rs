// crates/world_model/src/config.rs

use serde::{Deserialize, Serialize};

/// Configuration for the World Model Engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldModelConfig {
    /// Maximum BFS hops during belief propagation.
    /// Higher values allow influence to travel further but cost more CPU.
    pub max_propagation_hops: usize,

    /// Damping factor applied to the propagated log-odds delta at each hop [0, 1].
    /// 0 = no propagation beyond one hop; 1 = no attenuation (use with care).
    pub propagation_damping: f64,

    /// Minimum absolute log-odds delta that triggers further propagation.
    /// Prevents negligible updates from traversing the entire graph.
    pub propagation_threshold: f64,

    /// Minimum |world_prob − market_prob| required to emit a `WorldSignal`.
    pub min_signal_edge: f64,
}

impl Default for WorldModelConfig {
    fn default() -> Self {
        Self {
            max_propagation_hops: 4,
            propagation_damping: 0.6,
            propagation_threshold: 1e-6,
            min_signal_edge: 0.01,
        }
    }
}

impl WorldModelConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !(0.0..=1.0).contains(&self.propagation_damping) {
            return Err(format!(
                "propagation_damping must be in [0, 1], got {}",
                self.propagation_damping
            ));
        }
        if self.max_propagation_hops == 0 {
            return Err("max_propagation_hops must be > 0".to_string());
        }
        // Use !(x >= 0.0) instead of x < 0.0 so that NaN is also rejected
        // (IEEE 754: NaN < 0.0 is false, but !(NaN >= 0.0) is true).
        if !(self.propagation_threshold >= 0.0) {
            return Err(format!(
                "propagation_threshold must be >= 0, got {}",
                self.propagation_threshold
            ));
        }
        if !(0.0..=1.0).contains(&self.min_signal_edge) {
            return Err(format!(
                "min_signal_edge must be in [0, 1], got {}",
                self.min_signal_edge
            ));
        }
        Ok(())
    }
}
