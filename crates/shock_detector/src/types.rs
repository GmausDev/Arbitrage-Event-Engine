// crates/shock_detector/src/types.rs
//
// Internal state types for the Information Shock Detector.

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Utc};

// ---------------------------------------------------------------------------
// Per-market rolling state
// ---------------------------------------------------------------------------

/// Rolling state maintained for a single market.
#[derive(Debug, Clone)]
pub struct MarketState {
    /// Most recently observed probability for this market.
    pub last_prob: f64,
    /// Sliding window of successive price deltas (new_prob − old_prob).
    /// Used to estimate the baseline mean and standard deviation for z-scoring.
    pub delta_history: VecDeque<f64>,
    /// Most recent sentiment score seen for this market, if any.
    pub last_sentiment_score: f64,
    /// UTC timestamp of the most recent sentiment observation.
    pub last_sentiment_ts: Option<DateTime<Utc>>,
    /// `true` once at least one `MarketUpdate` has established the real
    /// initial price.  `false` when the entry was created by a
    /// `SentimentUpdate` before any price data arrived; in that case
    /// `last_prob` is a placeholder (0.5) that must not be used for delta
    /// computation.
    pub price_initialized: bool,
    /// UTC timestamp of the most recent update (price or sentiment).
    /// Used for LRU eviction when `max_tracked_markets` is reached.
    pub last_seen: DateTime<Utc>,
}

impl MarketState {
    /// Create from a confirmed market-price observation.
    pub fn new(initial_prob: f64, now: DateTime<Utc>) -> Self {
        Self {
            last_prob: initial_prob,
            delta_history: VecDeque::new(),
            last_sentiment_score: 0.0,
            last_sentiment_ts: None,
            price_initialized: true,
            last_seen: now,
        }
    }

    /// Create as a placeholder when a `SentimentUpdate` references a market
    /// that has not yet been seen via `MarketUpdate`.  Price fields are
    /// populated once the first real price observation arrives.
    pub fn new_from_sentiment(now: DateTime<Utc>) -> Self {
        Self {
            last_prob: 0.5, // placeholder — must not be used for delta until `price_initialized`
            delta_history: VecDeque::new(),
            last_sentiment_score: 0.0,
            last_sentiment_ts: None,
            price_initialized: false,
            last_seen: now,
        }
    }

    /// Push a new delta into the rolling window, evicting the oldest entry
    /// when the window is full.
    pub fn push_delta(&mut self, delta: f64, window: usize) {
        if self.delta_history.len() >= window {
            self.delta_history.pop_front();
        }
        self.delta_history.push_back(delta);
    }

    /// Sample mean of the delta history.  Returns `0.0` when empty.
    pub fn mean_delta(&self) -> f64 {
        if self.delta_history.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.delta_history.iter().sum();
        sum / self.delta_history.len() as f64
    }

    /// Sample standard deviation of the delta history.  Returns `None` when
    /// fewer than two observations are available (z-score undefined).
    pub fn stddev_delta(&self) -> Option<f64> {
        let n = self.delta_history.len();
        if n < 2 {
            return None;
        }
        let mean = self.mean_delta();
        let variance = self
            .delta_history
            .iter()
            .map(|d| (d - mean).powi(2))
            .sum::<f64>()
            / (n - 1) as f64;
        Some(variance.sqrt())
    }
}

// ---------------------------------------------------------------------------
// Engine-wide state
// ---------------------------------------------------------------------------

/// Mutable state maintained by the `InformationShockDetector` across events.
#[derive(Debug, Clone, Default)]
pub struct ShockDetectorState {
    /// Per-market rolling state, keyed by market ID.
    pub markets: HashMap<String, MarketState>,
    /// Total events processed since engine start.
    pub events_processed: u64,
    /// Total shocks detected (attempted to publish) since engine start.
    /// Note: counts detection events, not confirmed receipts by subscribers.
    pub shocks_detected: u64,
}

impl ShockDetectorState {
    /// Insert or update a market entry, evicting the least-recently-seen
    /// market when `max_tracked_markets > 0` and the map is at capacity.
    pub fn upsert_market(
        &mut self,
        market_id: String,
        state: MarketState,
        max_tracked_markets: usize,
    ) {
        if max_tracked_markets > 0
            && !self.markets.contains_key(&market_id)
            && self.markets.len() >= max_tracked_markets
        {
            // Evict the market with the oldest last_seen timestamp.
            if let Some(oldest_key) = self
                .markets
                .iter()
                .min_by_key(|(_, v)| v.last_seen)
                .map(|(k, _)| k.clone())
            {
                self.markets.remove(&oldest_key);
            }
        }
        self.markets.insert(market_id, state);
    }
}
