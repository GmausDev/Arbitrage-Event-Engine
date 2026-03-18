// agents/temporal_agent/src/state.rs

use std::{collections::HashMap, collections::VecDeque, sync::Arc};
use tokio::sync::RwLock;

/// Rolling price history for a single market.
#[derive(Debug, Clone, Default)]
pub struct MarketHistory {
    /// Recent prices, bounded by `config.history_window`.
    pub prices: VecDeque<f64>,
}

impl MarketHistory {
    /// Push a new price, evicting the oldest if the window is full.
    pub fn push(&mut self, price: f64, window: usize) {
        if self.prices.len() >= window {
            self.prices.pop_front();
        }
        self.prices.push_back(price);
    }

    /// Arithmetic mean of all stored prices.
    pub fn mean(&self) -> f64 {
        let n = self.prices.len();
        if n == 0 {
            return 0.0;
        }
        self.prices.iter().sum::<f64>() / n as f64
    }

    /// Sample standard deviation of all stored prices.
    /// Returns `None` when fewer than 2 samples are available.
    pub fn stddev(&self) -> Option<f64> {
        let n = self.prices.len();
        if n < 2 {
            return None;
        }
        let m = self.mean();
        let variance = self.prices.iter().map(|p| (p - m).powi(2)).sum::<f64>() / (n - 1) as f64;
        Some(variance.sqrt())
    }
}

/// Shared mutable state for the Temporal Strategy Agent.
#[derive(Debug, Default)]
pub struct TemporalState {
    /// Per-market rolling price histories.
    pub histories: HashMap<String, MarketHistory>,
    /// Total events processed (for test synchronisation).
    pub events_processed: u64,
    /// Total signals emitted.
    pub signals_emitted: u64,
}

pub type SharedTemporalState = Arc<RwLock<TemporalState>>;

pub fn new_shared_state() -> SharedTemporalState {
    Arc::new(RwLock::new(TemporalState::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_and_stddev_correct() {
        let mut h = MarketHistory::default();
        h.prices = VecDeque::from(vec![0.4, 0.5, 0.6]);
        assert!((h.mean() - 0.5).abs() < 1e-10);
        let sd = h.stddev().unwrap();
        // variance = ((−0.1)^2 + 0 + 0.1^2) / 2 = 0.01
        assert!((sd - 0.1).abs() < 1e-9);
    }

    #[test]
    fn stddev_none_with_single_sample() {
        let mut h = MarketHistory::default();
        h.prices = VecDeque::from(vec![0.5]);
        assert!(h.stddev().is_none());
    }

    #[test]
    fn push_evicts_oldest_when_full() {
        let mut h = MarketHistory::default();
        for i in 0..5 {
            h.push(i as f64, 5);
        }
        assert_eq!(h.prices.len(), 5);
        h.push(99.0, 5);
        assert_eq!(h.prices.len(), 5);
        assert_eq!(*h.prices.front().unwrap(), 1.0); // 0.0 was evicted
    }
}
