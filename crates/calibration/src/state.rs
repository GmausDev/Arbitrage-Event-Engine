// crates/calibration/src/state.rs

use std::collections::{HashMap, HashSet};

use crate::{
    config::CalibrationConfig,
    types::{BrierRecord, CalibrationBucket, PendingPrediction},
};

pub struct CalibrationState {
    pub config:   CalibrationConfig,
    /// Pending predictions keyed by market_id.  Only the most recent prediction
    /// per market is retained (upsert semantics).
    pub pending:  HashMap<String, PendingPrediction>,
    /// All scored Brier records.
    pub records:  Vec<BrierRecord>,
    /// Markets already resolved — prevents double-scoring from repeated extreme prices.
    pub resolved: HashSet<String>,
}

impl CalibrationState {
    pub fn new(config: CalibrationConfig) -> Self {
        Self {
            config,
            pending:  HashMap::new(),
            records:  Vec::new(),
            resolved: HashSet::new(),
        }
    }

    /// Store or update a prediction for `market_id`.
    pub fn on_posterior(&mut self, market_id: &str, predicted_prob: f64, timestamp: chrono::DateTime<chrono::Utc>) {
        if self.resolved.contains(market_id) {
            return; // already resolved — no point tracking further predictions
        }
        self.pending.insert(market_id.to_string(), PendingPrediction {
            market_id:      market_id.to_string(),
            predicted_prob,
            timestamp,
        });
    }

    /// Check whether `probability` signals resolution.  If so, and we have a
    /// pending prediction, score it and return a `BrierRecord`.
    pub fn on_market_update(&mut self, market_id: &str, probability: f64) -> Option<BrierRecord> {
        if self.resolved.contains(market_id) {
            return None;
        }

        let outcome = if probability <= self.config.resolution_epsilon {
            0.0 // resolved NO
        } else if probability >= 1.0 - self.config.resolution_epsilon {
            1.0 // resolved YES
        } else {
            return None; // not yet resolved
        };

        // Mark resolved before removing from pending so re-entrant calls are safe.
        self.resolved.insert(market_id.to_string());

        let pending = self.pending.remove(market_id)?;
        let brier_score = (pending.predicted_prob - outcome).powi(2);
        let record = BrierRecord {
            market_id:      market_id.to_string(),
            predicted_prob: pending.predicted_prob,
            outcome,
            brier_score,
        };
        self.records.push(record.clone());
        Some(record)
    }

    /// Overall mean Brier score across all scored predictions.
    pub fn overall_brier(&self) -> f64 {
        if self.records.is_empty() { return 0.5; } // uninformative prior
        self.records.iter().map(|r| r.brier_score).sum::<f64>() / self.records.len() as f64
    }

    /// Expected Calibration Error — weighted average of |mean_predicted − fraction_positive|
    /// across buckets with at least `min_bucket_samples` records.
    pub fn calibration_error(&self) -> f64 {
        let buckets = self.compute_buckets();
        let total: usize = buckets.iter().map(|b| b.count).sum();
        if total == 0 { return 0.0; }

        let ece: f64 = buckets.iter()
            .filter(|b| b.count >= self.config.min_bucket_samples)
            .map(|b| (b.mean_predicted - b.fraction_positive).abs() * b.count as f64)
            .sum::<f64>()
            / total as f64;
        ece
    }

    /// Compute calibration buckets from the scored records.
    pub fn compute_buckets(&self) -> Vec<CalibrationBucket> {
        let n = self.config.num_buckets;
        let width = 1.0 / n as f64;

        let mut buckets: Vec<(f64, usize, f64)> = (0..n)
            .map(|i| (i as f64 * width + width / 2.0, 0, 0.0))
            .collect();

        for rec in &self.records {
            let idx = ((rec.predicted_prob / width) as usize).min(n - 1);
            buckets[idx].1 += 1;
            buckets[idx].2 += rec.outcome;
        }

        // Also accumulate mean_predicted per bucket.
        let mut mean_predicted = vec![0.0f64; n];
        for rec in &self.records {
            let idx = ((rec.predicted_prob / width) as usize).min(n - 1);
            mean_predicted[idx] += rec.predicted_prob;
        }

        buckets.into_iter().enumerate().map(|(i, (center, count, yes_sum))| {
            let fraction_positive = if count > 0 { yes_sum / count as f64 } else { 0.0 };
            let mp                = if count > 0 { mean_predicted[i] / count as f64 } else { center };
            CalibrationBucket { center, mean_predicted: mp, fraction_positive, count }
        }).collect()
    }
}
