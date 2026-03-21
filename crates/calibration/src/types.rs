// crates/calibration/src/types.rs

use chrono::{DateTime, Utc};

/// A pending prediction waiting to be scored against a resolution outcome.
#[derive(Debug, Clone)]
pub struct PendingPrediction {
    pub market_id:      String,
    /// The posterior probability the model held at signal time.
    pub predicted_prob: f64,
    pub timestamp:      DateTime<Utc>,
}

/// A completed Brier score record.
#[derive(Debug, Clone)]
pub struct BrierRecord {
    pub market_id:      String,
    pub predicted_prob: f64,
    /// 1.0 = YES resolved, 0.0 = NO resolved.
    pub outcome:        f64,
    /// (predicted − outcome)²
    pub brier_score:    f64,
}

/// One calibration bucket (equal-width bin over [0, 1]).
#[derive(Debug, Clone)]
pub struct CalibrationBucket {
    /// Centre of the bucket (e.g. 0.05 for the [0.0, 0.1) bin).
    pub center:            f64,
    /// Mean predicted probability within this bucket.
    pub mean_predicted:    f64,
    /// Fraction of events in this bucket that resolved YES.
    pub fraction_positive: f64,
    /// Number of resolved predictions in this bucket.
    pub count:             usize,
}
