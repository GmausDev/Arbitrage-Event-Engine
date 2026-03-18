// crates/signal_priority_engine/src/config.rs

use serde::{Deserialize, Serialize};

/// Configuration for `SignalPriorityEngine`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriorityConfig {
    /// Milliseconds to buffer slow signals before flushing as `TopSignalsBatch`.
    /// Default: 500 ms — shorter than portfolio_optimizer tick to ensure it
    /// receives pre-ranked batches before its own tick fires.
    pub batch_window_ms: u64,

    /// Maximum number of slow signals emitted per `TopSignalsBatch` flush.
    /// Signals with the lowest priority score are dropped when the queue exceeds this.
    /// Default: 20.
    pub top_n: usize,

    /// `expected_value × confidence` threshold above which a signal is
    /// classified as `Fast` and emitted immediately via `Event::FastSignal`.
    /// Default: 0.04 (EV=0.08, confidence=0.5 → RAEV=0.04).
    pub fast_raev_threshold: f64,

    /// Minimum expected value for a slow signal to be admitted to the queue
    /// (pre-filter before RAEV ranking). Default: 0.01.
    pub min_slow_ev: f64,

    /// Maximum number of signals held in the slow queue between flushes.
    /// Excess signals (lowest score) are evicted. Default: 200.
    pub slow_queue_capacity: usize,
}

impl Default for PriorityConfig {
    fn default() -> Self {
        Self {
            batch_window_ms:    500,
            top_n:              20,
            fast_raev_threshold: 0.04,
            min_slow_ev:        0.01,
            slow_queue_capacity: 200,
        }
    }
}

impl PriorityConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.batch_window_ms == 0 {
            return Err("batch_window_ms must be > 0".into());
        }
        if self.top_n == 0 {
            return Err("top_n must be > 0".into());
        }
        if !self.fast_raev_threshold.is_finite() || self.fast_raev_threshold <= 0.0 {
            return Err("fast_raev_threshold must be positive and finite".into());
        }
        if self.slow_queue_capacity == 0 {
            return Err("slow_queue_capacity must be > 0".into());
        }
        if self.min_slow_ev < 0.0 {
            return Err("min_slow_ev must be >= 0.0".into());
        }
        Ok(())
    }
}
