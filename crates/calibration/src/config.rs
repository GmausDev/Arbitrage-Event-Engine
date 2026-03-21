// crates/calibration/src/config.rs

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CalibrationConfig {
    /// Probability epsilon for resolution detection.
    /// A market is considered resolved when its price is within this distance
    /// of 0.0 (NO) or 1.0 (YES).  Default: 0.02.
    pub resolution_epsilon: f64,

    /// Number of equal-width calibration buckets over [0, 1].  Default: 10.
    pub num_buckets: usize,

    /// Minimum samples per bucket before the bucket is included in ECE.
    /// Default: 5.
    pub min_bucket_samples: usize,
}

impl Default for CalibrationConfig {
    fn default() -> Self {
        Self {
            resolution_epsilon:  0.02,
            num_buckets:         10,
            min_bucket_samples:  5,
        }
    }
}
