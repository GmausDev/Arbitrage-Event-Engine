// crates/shock_detector/src/engine.rs
//
// Information Shock Detector — async event loop.
//
// ## Pipeline position
//
// ```text
// market_scanner  ──→ Event::Market
//                          └─→ InformationShockDetector   ← this module
// news_agent      ──→ Event::Sentiment
//                          └─→ InformationShockDetector
//
// InformationShockDetector ──→ Event::Shock   (to strategy agents, bayesian_engine)
// ```
//
// ## Detection summary
//
// **Price shocks** — on each `MarketUpdate`, a rolling z-score of successive
// probability deltas is maintained per market.  When |z| exceeds the
// configured threshold, an `InformationShock` is emitted.
//
// **Sentiment shocks** — on each `SentimentUpdate`, every related market
// whose |sentiment_score| exceeds the configured threshold receives a shock.
//
// **Fusion** — when a price shock fires and a fresh sentiment score exists for
// the same market, the two magnitudes are blended using `config.price_weight`.
//
// ## Lock discipline
//
// The write-lock is held only for state mutation (O(1) per market) and
// immediately released before any publish call.

use std::sync::Arc;

use chrono::Utc;
use common::{Event, EventBus, MarketUpdate, SentimentUpdate};
use metrics::counter;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    config::ShockDetectorConfig,
    detector::{detect_price_shock, detect_sentiment_shock},
    types::{MarketState, ShockDetectorState},
};

// ---------------------------------------------------------------------------
// Shared state type alias
// ---------------------------------------------------------------------------

pub type SharedShockState = Arc<tokio::sync::RwLock<ShockDetectorState>>;

fn new_shared_state() -> SharedShockState {
    Arc::new(tokio::sync::RwLock::new(ShockDetectorState::default()))
}

// ---------------------------------------------------------------------------
// InformationShockDetector
// ---------------------------------------------------------------------------

pub struct InformationShockDetector {
    pub config: ShockDetectorConfig,
    /// Shared mutable state — `pub` so tests can pre-seed or inspect.
    pub state: SharedShockState,
    bus: EventBus,
    /// Subscription stored as `Option` so `run()` can `.take()` it without
    /// needing `&mut self`.  Subscribed eagerly in `new()` to eliminate
    /// subscribe-after-spawn race conditions.
    rx: Option<broadcast::Receiver<Arc<Event>>>,
}

impl InformationShockDetector {
    pub fn new(config: ShockDetectorConfig, bus: EventBus) -> Self {
        if let Err(e) = config.validate() {
            panic!("invalid ShockDetectorConfig: {e}");
        }
        let rx = bus.subscribe();
        Self {
            config,
            state: new_shared_state(),
            bus,
            rx: Some(rx),
        }
    }

    /// Clone the shared state handle for external inspection (e.g. in tests).
    pub fn state(&self) -> SharedShockState {
        self.state.clone()
    }

    // -----------------------------------------------------------------------
    // Async event loop
    // -----------------------------------------------------------------------

    pub async fn run(mut self, cancel: CancellationToken) {
        let mut rx = self.rx.take().expect("rx already consumed");
        info!("shock_detector: started");

        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    info!("shock_detector: cancelled, shutting down");
                    break;
                }
                result = rx.recv() => {
                    match result {
                        Ok(ev) => self.handle_event(ev.as_ref()).await,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(
                                "shock_detector: lagged by {n} events \
                                 — consider increasing bus capacity"
                            );
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("shock_detector: bus closed, shutting down");
                            break;
                        }
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Event dispatch
    // -----------------------------------------------------------------------

    async fn handle_event(&self, event: &Event) {
        match event {
            Event::Market(update) => self.on_market_update(update).await,
            Event::Sentiment(update) => self.on_sentiment_update(update).await,
            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // MarketUpdate — price-based shock detection
    // -----------------------------------------------------------------------

    async fn on_market_update(&self, update: &MarketUpdate) {
        counter!("shock_detector_market_updates_total").increment(1);

        let new_prob = update.market.probability;
        if !new_prob.is_finite() || !(0.0..=1.0).contains(&new_prob) {
            warn!(
                market_id = %update.market.id,
                prob = new_prob,
                "shock_detector: ignoring invalid probability from MarketUpdate"
            );
            return;
        }

        let market_id = update.market.id.clone();
        let window = self.config.rolling_window;
        let max_markets = self.config.max_tracked_markets;

        // Capture timestamp once before lock acquisition for consistency.
        let now = Utc::now();

        let shock = {
            let mut state = self.state.write().await;
            state.events_processed += 1;

            let is_new = !state.markets.contains_key(&market_id);

            if is_new {
                // First observation — establish baseline only.
                // No delta can be computed; z-scoring is impossible without history.
                // Use upsert_market so the cap+eviction policy is applied.
                let ms = MarketState::new(new_prob, now);
                state.upsert_market(market_id, ms, max_markets);
                None
            } else {
                let entry = state.markets.get_mut(&market_id).unwrap();

                let shock = if entry.price_initialized {
                    // Detect shock using the current (pre-update) state.
                    detect_price_shock(&market_id, entry, new_prob, &self.config, now)
                } else {
                    // Entry was created by a SentimentUpdate with a placeholder
                    // last_prob.  Establish the real baseline; skip delta.
                    None
                };

                // Update rolling state.
                if entry.price_initialized {
                    let delta = new_prob - entry.last_prob;
                    entry.push_delta(delta, window);
                }
                entry.last_prob = new_prob;
                entry.price_initialized = true;
                entry.last_seen = now;

                if shock.is_some() {
                    state.shocks_detected += 1;
                }
                shock
            }
        };
        // Lock released — safe to publish now.

        if let Some(s) = shock {
            debug!(
                market_id = %s.market_id,
                magnitude = s.magnitude,
                z_score   = s.z_score,
                direction = ?s.direction,
                "shock_detector: price shock detected"
            );
            self.publish_shock(s);
        }
    }

    // -----------------------------------------------------------------------
    // SentimentUpdate — sentiment-based shock detection
    // -----------------------------------------------------------------------

    async fn on_sentiment_update(&self, update: &SentimentUpdate) {
        counter!("shock_detector_sentiment_updates_total").increment(1);

        let score = update.sentiment_score;
        if !score.is_finite() {
            warn!(
                score,
                "shock_detector: ignoring non-finite sentiment score"
            );
            return;
        }
        let score = score.clamp(-1.0, 1.0);

        let now = Utc::now();
        let max_markets = self.config.max_tracked_markets;

        // Collect shocks for each related market, updating sentiment state.
        let shocks: Vec<_> = {
            let mut state = self.state.write().await;
            state.events_processed += 1;

            let mut collected = Vec::new();
            for market_id in &update.related_market_ids {
                // If the market is new, create a sentinel entry so that
                // when the first MarketUpdate arrives it can inherit the
                // sentiment reading correctly.  Eviction applies here too.
                if !state.markets.contains_key(market_id) {
                    let ms = MarketState::new_from_sentiment(now);
                    state.upsert_market(market_id.clone(), ms, max_markets);
                }

                let entry = state.markets.get_mut(market_id).unwrap();
                entry.last_sentiment_score = score;
                entry.last_sentiment_ts = Some(now);
                entry.last_seen = now;

                if let Some(shock) =
                    detect_sentiment_shock(market_id, score, &self.config, now)
                {
                    state.shocks_detected += 1;
                    collected.push(shock);
                }
            }
            collected
        };
        // Lock released.

        for shock in shocks {
            debug!(
                market_id = %shock.market_id,
                magnitude = shock.magnitude,
                direction = ?shock.direction,
                "shock_detector: sentiment shock detected"
            );
            self.publish_shock(shock);
        }
    }

    // -----------------------------------------------------------------------
    // Publishing
    // -----------------------------------------------------------------------

    fn publish_shock(&self, shock: common::InformationShock) {
        counter!("shock_detector_shocks_emitted_total").increment(1);
        if let Err(e) = self.bus.publish(Event::Shock(shock)) {
            warn!("shock_detector: failed to publish InformationShock: {e}");
        }
    }
}
