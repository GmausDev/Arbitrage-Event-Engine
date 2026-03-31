// crates/data_ingestion/src/ingestion_engine.rs
//
// DataIngestionEngine — top-level coordinator for the data ingestion layer.
//
// Responsibilities
// ────────────────
//   1. Build a `SnapshotCache` (shared Arc<RwLock<...>>).
//   2. Create `PollerTask` entries for each enabled connector category.
//   3. Register streaming sources with `StreamManager`.
//   4. Run `PollingManager` and `StreamManager` concurrently until shutdown.
//
// The engine is the sole producer of:
//   Event::Market, Event::MarketSnapshot, Event::NewsEvent,
//   Event::SocialTrend, Event::EconomicRelease, Event::CalendarEvent

use std::sync::Arc;

use async_trait::async_trait;
use futures::future::join_all;
use metrics::counter;
use tokio::sync::RwLock;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::info;

use common::{Event, EventBus};

use crate::{
    cache::snapshot_cache::SnapshotCache,
    config::IngestionConfig,
    matcher::match_news_to_markets,
    connectors::{
        calendar_connectors::{CalendarConnector, PredictItCalendarConnector},
        economic_connectors::{EconomicConnector, FREDConnector, TradingEconomicsConnector},
        market_connectors::{KalshiIngestionConnector, MarketConnector, PolymarketIngestionConnector},
        news_connectors::{AlphaVantageNewsConnector, NewsApiConnector, NewsConnector},
        social_connectors::{RedditConnector, SocialConnector, TwitterConnector},
    },
    normalization::{
        economic_normalizer::EconomicNormalizer,
        market_normalizer::MarketNormalizer,
        news_normalizer::NewsNormalizer,
        social_normalizer::SocialNormalizer,
    },
    scheduler::{
        polling_manager::{ConnectorRunner, PollerTask, PollingManager},
        rate_limiter::RateLimiter,
    },
    streaming::{
        stream_manager::{StreamManager, StreamSpec},
        websocket_client::WebSocketConfig,
    },
};

// ---------------------------------------------------------------------------
// Connector runner implementations
// ---------------------------------------------------------------------------

/// Polls market connectors, normalises output, updates cache, publishes events.
struct MarketPollerRunner {
    connectors: Vec<Box<dyn MarketConnector>>,
}

#[async_trait]
impl ConnectorRunner for MarketPollerRunner {
    async fn run_once(
        &self,
        bus: EventBus,
        cache: Arc<RwLock<SnapshotCache>>,
    ) -> anyhow::Result<usize> {
        // Run all connectors concurrently to reduce latency.
        let market_futs = self.connectors.iter().map(|c| c.fetch_markets());
        let book_futs = self.connectors.iter().map(|c| c.fetch_orderbooks());

        let market_results = join_all(market_futs).await;
        let book_results = join_all(book_futs).await;

        let mut all_books = vec![];

        // Normalise per-connector so each snapshot can be tagged with its
        // source platform name before all batches are merged.
        let mut snapshots: Vec<common::events::MarketSnapshot> = Vec::new();
        for (conn, result) in self.connectors.iter().zip(market_results) {
            match result {
                Ok(markets) => {
                    let platform = conn.name().to_string();
                    let mut batch = MarketNormalizer::normalize_markets(markets);
                    for s in &mut batch {
                        s.source_platform = platform.clone();
                    }
                    snapshots.extend(batch);
                }
                Err(e) => tracing::warn!(connector = conn.name(), err = %e, "market fetch failed"),
            }
        }
        for (conn, result) in self.connectors.iter().zip(book_results) {
            match result {
                Ok(books) => all_books.extend(books),
                Err(e) => tracing::warn!(connector = conn.name(), err = %e, "orderbook fetch failed"),
            }
        }

        snapshots = MarketNormalizer::apply_orderbooks(snapshots, all_books);
        let updates = MarketNormalizer::to_market_updates(&snapshots);
        let count = snapshots.len();

        {
            let mut c = cache.write().await;
            for s in &snapshots {
                c.update_market(s.clone());
            }
        }

        for snapshot in snapshots {
            let _ = bus.publish(Event::MarketSnapshot(snapshot));
            counter!("data_ingestion_market_snapshots_total").increment(1);
        }
        for update in updates {
            let _ = bus.publish(Event::Market(update));
            counter!("data_ingestion_market_updates_total").increment(1);
        }

        Ok(count)
    }
}

/// Polls news connectors, normalises, deduplicates, caches, and publishes.
struct NewsPollerRunner {
    connectors: Vec<Box<dyn NewsConnector>>,
}

#[async_trait]
impl ConnectorRunner for NewsPollerRunner {
    async fn run_once(
        &self,
        bus: EventBus,
        cache: Arc<RwLock<SnapshotCache>>,
    ) -> anyhow::Result<usize> {
        let mut all_raw = vec![];
        for conn in &self.connectors {
            match conn.fetch_headlines().await {
                Ok(items) => all_raw.extend(items),
                Err(e) => tracing::warn!(connector = conn.name(), err = %e, "news fetch failed"),
            }
        }

        let events = NewsNormalizer::dedup(NewsNormalizer::normalize(all_raw));
        let mut new_count = 0;

        // Collect new items inside a single write-lock scope, then publish all
        // outside the lock.  This avoids repeated lock acquisitions and ensures
        // the lock is never held across an async publish call.
        let to_publish: Vec<_> = {
            let mut guard = cache.write().await;
            events
                .into_iter()
                .filter(|e| {
                    if guard.has_news_id(&e.id) {
                        return false;
                    }
                    guard.push_news(e.clone());
                    true
                })
                .collect()
        }; // write lock dropped here

        // Snapshot current market titles outside the lock for entity matching.
        let market_snapshots: Vec<_> = {
            let guard = cache.read().await;
            guard.all_markets().cloned().collect()
        };

        for event in to_publish {
            // Entity → market matching: if any entity/topic matches a market
            // title, emit Event::Sentiment so bayesian_engine can update
            // posteriors for the relevant markets.
            if let Some(sentiment) = match_news_to_markets(&event, &market_snapshots) {
                let matched = sentiment.related_market_ids.len();
                let _ = bus.publish(Event::Sentiment(sentiment));
                counter!("data_ingestion_sentiment_matches_total").increment(matched as u64);
            }
            let _ = bus.publish(Event::NewsEvent(event));
            counter!("data_ingestion_news_events_total").increment(1);
            new_count += 1;
        }

        Ok(new_count)
    }
}

/// Polls social connectors, normalises, updates cache, publishes.
struct SocialPollerRunner {
    connectors: Vec<Box<dyn SocialConnector>>,
}

#[async_trait]
impl ConnectorRunner for SocialPollerRunner {
    async fn run_once(
        &self,
        bus: EventBus,
        cache: Arc<RwLock<SnapshotCache>>,
    ) -> anyhow::Result<usize> {
        // Run all connectors concurrently.
        let futs = self.connectors.iter().map(|c| c.stream_posts());
        let results = join_all(futs).await;

        let mut all_raw = vec![];
        for (conn, result) in self.connectors.iter().zip(results) {
            match result {
                Ok(posts) => all_raw.extend(posts),
                Err(e) => tracing::warn!(connector = conn.name(), err = %e, "social fetch failed"),
            }
        }

        let trends = SocialNormalizer::normalize(all_raw);
        let count = trends.len();

        {
            let mut c = cache.write().await;
            for t in &trends {
                c.update_trend(t.clone());
            }
        }

        for trend in trends {
            let _ = bus.publish(Event::SocialTrend(trend));
            counter!("data_ingestion_social_trends_total").increment(1);
        }

        Ok(count)
    }
}

/// Polls economic connectors + calendar connectors, normalises, publishes.
struct EconomicPollerRunner {
    econ_connectors: Vec<Box<dyn EconomicConnector>>,
    cal_connectors: Vec<Box<dyn CalendarConnector>>,
    calendar_lookahead_days: u32,
}

#[async_trait]
impl ConnectorRunner for EconomicPollerRunner {
    async fn run_once(
        &self,
        bus: EventBus,
        cache: Arc<RwLock<SnapshotCache>>,
    ) -> anyhow::Result<usize> {
        let mut count = 0;

        // Economic releases — fetch all connectors concurrently.
        let econ_results = join_all(self.econ_connectors.iter().map(|c| c.fetch_releases())).await;
        let mut all_econ = vec![];
        for (conn, result) in self.econ_connectors.iter().zip(econ_results) {
            match result {
                Ok(data) => all_econ.extend(data),
                Err(e) => tracing::warn!(connector = conn.name(), err = %e, "economic fetch failed"),
            }
        }
        let releases = EconomicNormalizer::normalize_releases(all_econ);
        {
            let mut c = cache.write().await;
            for r in &releases {
                c.update_economic(r.clone());
            }
        }
        for release in releases {
            let _ = bus.publish(Event::EconomicRelease(release));
            counter!("data_ingestion_economic_releases_total").increment(1);
            count += 1;
        }

        // Calendar events — fetch all connectors concurrently.
        let lookahead = self.calendar_lookahead_days;
        let cal_results = join_all(self.cal_connectors.iter().map(|c| c.fetch_events(lookahead))).await;
        let mut all_cal = vec![];
        for (conn, result) in self.cal_connectors.iter().zip(cal_results) {
            match result {
                Ok(entries) => all_cal.extend(entries),
                Err(e) => tracing::warn!(connector = conn.name(), err = %e, "calendar fetch failed"),
            }
        }
        let cal_events = EconomicNormalizer::normalize_calendar(all_cal);
        {
            let mut c = cache.write().await;
            for e in &cal_events {
                c.update_calendar(e.clone());
            }
        }
        for cal_event in cal_events {
            let _ = bus.publish(Event::CalendarEvent(cal_event));
            counter!("data_ingestion_calendar_events_total").increment(1);
            count += 1;
        }

        Ok(count)
    }
}

// ---------------------------------------------------------------------------
// DataIngestionEngine
// ---------------------------------------------------------------------------

pub struct DataIngestionEngine {
    pub config: IngestionConfig,
    /// Shared snapshot cache — `pub` so callers (e.g. tests) can pre-seed or read.
    pub cache: Arc<RwLock<SnapshotCache>>,
    bus: EventBus,
}

impl DataIngestionEngine {
    pub fn new(config: IngestionConfig, bus: EventBus) -> Result<Self, anyhow::Error> {
        config
            .validate()
            .map_err(|e| anyhow::anyhow!("invalid IngestionConfig: {e}"))?;
        let cache = Arc::new(RwLock::new(SnapshotCache::new(
            config.cache_max_news_items,
            config.cache_max_trend_items,
        )));
        Ok(Self { config, cache, bus })
    }

    pub async fn run(self, cancel: CancellationToken) {
        info!(
            market_enabled  = self.config.market.enabled,
            news_enabled    = self.config.news.enabled,
            social_enabled  = self.config.social.enabled,
            economic_enabled = self.config.economic.enabled,
            calendar_enabled = self.config.calendar.enabled,
            "data_ingestion_engine: starting"
        );

        let mut polling = PollingManager::new(self.bus.clone(), self.cache.clone());
        let mut streaming = StreamManager::new(self.bus.clone(), self.cache.clone());

        // ── Market connector polling ──────────────────────────────────────
        if self.config.market.enabled {
            let poly = PolymarketIngestionConnector::with_config(
                self.config.market.timeout_ms,
                self.config.polymarket_base_url.clone(),
                self.config.market.max_retries,
                self.config.market.retry_base_delay_ms,
            ).unwrap_or_else(|e| {
                tracing::warn!("polymarket client init failed: {e}");
                PolymarketIngestionConnector::default()
            });
            let kalshi = KalshiIngestionConnector::with_config(
                self.config.market.timeout_ms,
                self.config.kalshi_base_url.clone(),
                self.config.market.max_retries,
                self.config.market.retry_base_delay_ms,
            ).unwrap_or_else(|e| {
                tracing::warn!("kalshi client init failed: {e}");
                KalshiIngestionConnector::default()
            });

            // Register streaming URLs if available
            for (name, url) in [("polymarket_ws", poly.stream_url()), ("kalshi_ws", kalshi.stream_url())] {
                if let Some(url) = url {
                    streaming.add_stream(StreamSpec {
                        name: name.into(),
                        config: WebSocketConfig {
                            url,
                            reconnect_delay_ms: self.config.stream_reconnect_delay_ms,
                            max_retries: self.config.stream_max_retries,
                            ..WebSocketConfig::default()
                        },
                    });
                }
            }

            polling.add_task(PollerTask {
                name: "market".into(),
                poll_interval: Duration::from_millis(self.config.market.poll_interval_ms),
                rate_limiter: RateLimiter::new(
                    self.config.market.rate_limit_rps,
                    self.config.market.rate_limit_rps.max(1.0),
                ),
                runner: Box::new(MarketPollerRunner {
                    connectors: vec![Box::new(poly), Box::new(kalshi)],
                }),
            });
        }

        // ── News connector polling ────────────────────────────────────────
        if self.config.news.enabled {
            let news_api = NewsApiConnector::with_config(
                self.config.news.timeout_ms,
                self.config.newsapi_base_url.clone(),
                self.config.news.max_retries,
                self.config.news.retry_base_delay_ms,
            ).unwrap_or_default();
            let av_news = AlphaVantageNewsConnector::with_config(
                self.config.news.timeout_ms,
                self.config.alphavantage_base_url.clone(),
                self.config.news.max_retries,
                self.config.news.retry_base_delay_ms,
            ).unwrap_or_default();
            polling.add_task(PollerTask {
                name: "news".into(),
                poll_interval: Duration::from_millis(self.config.news.poll_interval_ms),
                rate_limiter: RateLimiter::new(
                    self.config.news.rate_limit_rps,
                    self.config.news.rate_limit_rps.max(1.0),
                ),
                runner: Box::new(NewsPollerRunner {
                    connectors: vec![Box::new(news_api), Box::new(av_news)],
                }),
            });
        }

        // ── Social connector polling ──────────────────────────────────────
        if self.config.social.enabled {
            let reddit = RedditConnector::with_config(
                self.config.social.timeout_ms,
                self.config.reddit_base_url.clone(),
                self.config.reddit_subreddits.clone(),
                self.config.social.max_retries,
                self.config.social.retry_base_delay_ms,
            ).unwrap_or_else(|e| {
                tracing::warn!("reddit client init failed: {e}");
                RedditConnector::default()
            });
            let twitter = TwitterConnector::with_config(
                self.config.social.timeout_ms,
                self.config.twitter_base_url.clone(),
                self.config.social.max_retries,
                self.config.social.retry_base_delay_ms,
            ).unwrap_or_default();
            polling.add_task(PollerTask {
                name: "social".into(),
                poll_interval: Duration::from_millis(self.config.social.poll_interval_ms),
                rate_limiter: RateLimiter::new(
                    self.config.social.rate_limit_rps,
                    self.config.social.rate_limit_rps.max(1.0),
                ),
                runner: Box::new(SocialPollerRunner {
                    connectors: vec![Box::new(reddit), Box::new(twitter)],
                }),
            });
        }

        // ── Economic + calendar connector polling ─────────────────────────
        if self.config.economic.enabled || self.config.calendar.enabled {
            let fred = FREDConnector::with_config(
                self.config.economic.timeout_ms,
                self.config.fred_base_url.clone(),
                self.config.fred_series.clone(),
                self.config.economic.max_retries,
                self.config.economic.retry_base_delay_ms,
            ).unwrap_or_default();
            let te = TradingEconomicsConnector::with_config(
                self.config.economic.timeout_ms,
                self.config.tradingeconomics_base_url.clone(),
                self.config.economic.max_retries,
                self.config.economic.retry_base_delay_ms,
            ).unwrap_or_default();
            polling.add_task(PollerTask {
                name: "economic".into(),
                poll_interval: Duration::from_millis(self.config.economic.poll_interval_ms),
                rate_limiter: RateLimiter::new(
                    self.config.economic.rate_limit_rps,
                    self.config.economic.rate_limit_rps.max(1.0),
                ),
                runner: Box::new(EconomicPollerRunner {
                    econ_connectors: vec![Box::new(fred), Box::new(te)],
                    cal_connectors: vec![Box::new(PredictItCalendarConnector::new())],
                    calendar_lookahead_days: 90,
                }),
            });
        }

        // ── Run polling + streaming concurrently ──────────────────────────
        let cancel_poll = cancel.clone();
        let cancel_stream = cancel.clone();

        tokio::join!(
            polling.run(cancel_poll),
            streaming.run(cancel_stream),
        );

        info!("data_ingestion_engine: shut down");
    }
}
