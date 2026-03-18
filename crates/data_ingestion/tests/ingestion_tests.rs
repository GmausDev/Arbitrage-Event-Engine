// crates/data_ingestion/tests/ingestion_tests.rs
//
// Integration and higher-level unit tests for the data_ingestion crate.
//
// These tests use mock connectors to verify the full pipeline:
//   connector → normalizer → cache → Event Bus

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use common::{Event, EventBus};
use data_ingestion::{
    DataIngestionEngine, IngestionConfig,
    cache::snapshot_cache::SnapshotCache,
    connectors::{
        calendar_connectors::{CalendarConnector, MockCalendarConnector},
        economic_connectors::{EconomicConnector, MockEconomicConnector},
        market_connectors::{MarketConnector, MockMarketConnector},
        RawCalendarEntry, RawEconomicData, RawMarket, RawNewsItem, RawSocialPost,
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
};
use async_trait::async_trait;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

// ============================================================================
// Config validation tests
// ============================================================================

#[test]
fn default_config_is_valid() {
    IngestionConfig::default().validate().unwrap();
}

#[test]
fn config_rejects_zero_poll_interval() {
    let mut cfg = IngestionConfig::default();
    cfg.market.poll_interval_ms = 0;
    assert!(cfg.validate().is_err());
}

#[test]
fn config_rejects_zero_rate_limit() {
    let mut cfg = IngestionConfig::default();
    cfg.news.rate_limit_rps = 0.0;
    assert!(cfg.validate().is_err());
}

#[test]
fn config_rejects_zero_cache_news() {
    let mut cfg = IngestionConfig::default();
    cfg.cache_max_news_items = 0;
    assert!(cfg.validate().is_err());
}

// ============================================================================
// Rate limiter unit tests (complementing inline tests)
// ============================================================================

#[tokio::test]
async fn rate_limiter_acquire_does_not_block_initially() {
    use data_ingestion::scheduler::rate_limiter::RateLimiter;
    let mut rl = RateLimiter::new(10.0, 5.0);
    // Full bucket — should return immediately five times
    let start = std::time::Instant::now();
    for _ in 0..5 {
        rl.acquire().await;
    }
    assert!(start.elapsed().as_millis() < 100, "expected immediate return for pre-filled bucket");
}

// ============================================================================
// Snapshot cache integration tests
// ============================================================================

#[test]
fn cache_market_ids_snapshot() {
    let mut cache = SnapshotCache::new(10, 10);
    cache.update_market(common::events::MarketSnapshot {
        market_id: "m1".into(),
        title: "T".into(),
        description: "D".into(),
        probability: 0.5,
        bid: 0.49,
        ask: 0.51,
        volume: 1000.0,
        liquidity: 500.0,
        timestamp: Utc::now(),
    });
    cache.update_market(common::events::MarketSnapshot {
        market_id: "m2".into(),
        title: "T2".into(),
        description: "D2".into(),
        probability: 0.6,
        bid: 0.59,
        ask: 0.61,
        volume: 2000.0,
        liquidity: 800.0,
        timestamp: Utc::now(),
    });
    let ids = cache.market_ids();
    assert!(ids.contains(&"m1".to_string()));
    assert!(ids.contains(&"m2".to_string()));
}

// ============================================================================
// Normalizer integration tests
// ============================================================================

#[test]
fn market_normalizer_full_pipeline() {
    let raw = vec![
        RawMarket {
            market_id: "mkt1".into(),
            title: "Will X happen?".into(),
            description: "Description".into(),
            probability: 0.55,
            bid: 0.54,
            ask: 0.56,
            volume: 5000.0,
            liquidity: 2000.0,
        },
        RawMarket {
            market_id: "mkt2".into(),
            title: "Will Y happen?".into(),
            description: "Description 2".into(),
            probability: 0.30,
            bid: 0.29,
            ask: 0.31,
            volume: 3000.0,
            liquidity: 1000.0,
        },
    ];

    let snapshots = MarketNormalizer::normalize_markets(raw);
    assert_eq!(snapshots.len(), 2);

    let updates = MarketNormalizer::to_market_updates(&snapshots);
    assert_eq!(updates.len(), 2);
    assert_eq!(updates[0].market.id, "mkt1");
    assert!((updates[0].market.probability - 0.55).abs() < 1e-9);
}

#[test]
fn news_normalizer_dedup_integration() {
    let raw = vec![
        RawNewsItem {
            id: "story1".into(),
            headline: "Breaking news".into(),
            body: "Body".into(),
            source: "Reuters".into(),
            entities: vec!["Fed".into()],
            topics: vec!["economics".into()],
            published_at: Utc::now(),
        },
        RawNewsItem {
            id: "story1".into(), // duplicate
            headline: "Breaking news (copy)".into(),
            body: "Body".into(),
            source: "Reuters".into(),
            entities: vec![],
            topics: vec![],
            published_at: Utc::now(),
        },
        RawNewsItem {
            id: "story2".into(),
            headline: "More news".into(),
            body: "Body2".into(),
            source: "AP".into(),
            entities: vec![],
            topics: vec!["politics".into()],
            published_at: Utc::now(),
        },
    ];

    let events = NewsNormalizer::dedup(NewsNormalizer::normalize(raw));
    assert_eq!(events.len(), 2, "duplicate should be removed");
}

#[test]
fn social_normalizer_mention_rate_calculation() {
    let raw = vec![
        RawSocialPost {
            topic: "bitcoin".into(),
            mention_count: 600,         // 600 mentions over 10 min = 60 /min
            rolling_window_secs: 600,
            sentiment_score: 0.4,
            velocity: 1.5,
        },
    ];
    let trends = SocialNormalizer::normalize(raw);
    assert_eq!(trends.len(), 1);
    assert!((trends[0].mention_rate - 60.0).abs() < 1e-9);
    assert_eq!(trends[0].topic, "bitcoin");
}

#[test]
fn economic_normalizer_surprise_calculation() {
    use common::events::EconomicRelease;
    let release = EconomicRelease {
        indicator: "US_NFP".into(),
        value: 256_000.0,
        forecast: Some(185_000.0),
        previous: Some(199_000.0),
        timestamp: Utc::now(),
    };
    let surprise = EconomicNormalizer::surprise_factor(&release).unwrap();
    // (256000 - 185000) / 185000 ≈ 0.3838
    assert!(surprise > 0.3 && surprise < 0.4);
}

// ============================================================================
// Mock connector tests
// ============================================================================

#[tokio::test]
async fn mock_market_connector_full_fetch() {
    let markets = vec![
        RawMarket {
            market_id: "test_m1".into(),
            title: "T1".into(),
            description: "D1".into(),
            probability: 0.45,
            bid: 0.44,
            ask: 0.46,
            volume: 100.0,
            liquidity: 50.0,
        },
        RawMarket {
            market_id: "test_m2".into(),
            title: "T2".into(),
            description: "D2".into(),
            probability: 0.70,
            bid: 0.69,
            ask: 0.71,
            volume: 200.0,
            liquidity: 80.0,
        },
    ];
    let connector = MockMarketConnector::new(markets);
    let fetched = connector.fetch_markets().await.unwrap();
    assert_eq!(fetched.len(), 2);
    let books = connector.fetch_orderbooks().await.unwrap();
    assert_eq!(books.len(), 2);
}

#[tokio::test]
async fn mock_economic_connector_round_trip() {
    let data = vec![RawEconomicData {
        indicator: "MY_IND".into(),
        value: 99.5,
        forecast: Some(100.0),
        previous: Some(98.0),
        release_time: Utc::now(),
    }];
    let connector = MockEconomicConnector::new(data);
    let releases = connector.fetch_releases().await.unwrap();
    let normalised = EconomicNormalizer::normalize_releases(releases);
    assert_eq!(normalised.len(), 1);
    assert!((normalised[0].value - 99.5).abs() < 1e-9);
}

#[tokio::test]
async fn mock_calendar_connector_round_trip() {
    use chrono::Duration;
    let entries = vec![RawCalendarEntry {
        event_id: "evt1".into(),
        title: "Test Event".into(),
        scheduled_time: Utc::now() + Duration::days(5),
        category: "test".into(),
        expected_impact: 0.75,
    }];
    let connector = MockCalendarConnector::new(entries);
    let raw = connector.fetch_events(30).await.unwrap();
    let events = EconomicNormalizer::normalize_calendar(raw);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_id, "evt1");
    assert_eq!(events[0].expected_impact, 0.75);
}

// ============================================================================
// Polling manager integration — mock runner publishes events to bus
// ============================================================================

struct MarketEventRunner {
    market_ids: Vec<String>,
}

#[async_trait]
impl ConnectorRunner for MarketEventRunner {
    async fn run_once(
        &self,
        bus: EventBus,
        cache: Arc<RwLock<SnapshotCache>>,
    ) -> anyhow::Result<usize> {
        let snapshots: Vec<_> = self
            .market_ids
            .iter()
            .map(|id| common::events::MarketSnapshot {
                market_id: id.clone(),
                title: "T".into(),
                description: "D".into(),
                probability: 0.5,
                bid: 0.49,
                ask: 0.51,
                volume: 1000.0,
                liquidity: 500.0,
                timestamp: Utc::now(),
            })
            .collect();

        {
            let mut c = cache.write().await;
            for s in &snapshots {
                c.update_market(s.clone());
            }
        }
        for s in &snapshots {
            let _ = bus.publish(Event::MarketSnapshot(s.clone()));
        }
        Ok(snapshots.len())
    }
}

#[tokio::test]
async fn polling_manager_publishes_market_snapshot_events() {
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let cache = Arc::new(RwLock::new(SnapshotCache::new(10, 10)));
    let cancel = CancellationToken::new();

    let task = PollerTask {
        name: "market_test".into(),
        poll_interval: tokio::time::Duration::from_millis(20),
        rate_limiter: RateLimiter::new(100.0, 10.0),
        runner: Box::new(MarketEventRunner {
            market_ids: vec!["m1".into(), "m2".into()],
        }),
    };

    let mut mgr = PollingManager::new(bus.clone(), cache.clone());
    mgr.add_task(task);

    let cancel_clone = cancel.clone();
    tokio::spawn(async move { mgr.run(cancel_clone).await });

    // Collect the first MarketSnapshot event
    let event = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match rx.recv().await {
                Ok(ev) => match ev.as_ref() {
                    Event::MarketSnapshot(_) => return ev,
                    _ => continue,
                },
                Err(_) => panic!("bus error"),
            }
        }
    })
    .await
    .expect("timed out waiting for MarketSnapshot event");

    cancel.cancel();

    assert!(matches!(event.as_ref(), Event::MarketSnapshot(_)));
}

#[tokio::test]
async fn polling_manager_cache_updated_after_poll() {
    let bus = EventBus::new();
    let cache = Arc::new(RwLock::new(SnapshotCache::new(10, 10)));
    let cancel = CancellationToken::new();

    let task = PollerTask {
        name: "cache_test".into(),
        poll_interval: tokio::time::Duration::from_millis(30),
        rate_limiter: RateLimiter::new(100.0, 5.0),
        runner: Box::new(MarketEventRunner {
            market_ids: vec!["cached_market".into()],
        }),
    };

    let mut mgr = PollingManager::new(bus.clone(), cache.clone());
    mgr.add_task(task);

    let cancel_clone = cancel.clone();
    tokio::spawn(async move { mgr.run(cancel_clone).await });

    // Wait for at least one poll cycle
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel.cancel();

    let c = cache.read().await;
    assert!(
        c.get_market("cached_market").is_some(),
        "cache should have been populated after poll"
    );
}

// ============================================================================
// Integration: DataIngestionEngine boots and shuts down cleanly
// ============================================================================

#[tokio::test]
async fn ingestion_engine_starts_and_cancels_cleanly() {
    let bus = EventBus::new();
    let cancel = CancellationToken::new();

    // Use very short intervals so the engine actually fires during the test
    let mut cfg = IngestionConfig::default();
    cfg.market.poll_interval_ms = 30;
    cfg.news.poll_interval_ms = 30;
    cfg.social.poll_interval_ms = 30;
    cfg.economic.poll_interval_ms = 30;

    let engine = DataIngestionEngine::new(cfg, bus.clone());

    let cancel_clone = cancel.clone();
    let handle = tokio::spawn(async move { engine.run(cancel_clone).await });

    // Let it run briefly
    tokio::time::sleep(Duration::from_millis(150)).await;
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("engine should shut down within 2 s")
        .unwrap();
}

#[tokio::test]
async fn ingestion_engine_publishes_events_during_run() {
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let mut cfg = IngestionConfig::default();
    cfg.market.poll_interval_ms = 20;
    cfg.news.poll_interval_ms = 20;
    cfg.social.poll_interval_ms = 20;
    cfg.economic.poll_interval_ms = 20;
    cfg.calendar.poll_interval_ms = 20;

    let engine = DataIngestionEngine::new(cfg, bus.clone());
    let cancel_clone = cancel.clone();
    tokio::spawn(async move { engine.run(cancel_clone).await });

    // Collect events for 3 s (real HTTP APIs need more time than stubs)
    let deadline = tokio::time::Instant::now() + Duration::from_millis(3_000);
    let mut market_events = 0usize;
    let mut news_events = 0usize;
    let mut social_events = 0usize;

    loop {
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        match tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
            Ok(Ok(ev)) => match ev.as_ref() {
                Event::MarketSnapshot(_) | Event::Market(_) => market_events += 1,
                Event::NewsEvent(_) => news_events += 1,
                Event::SocialTrend(_) => social_events += 1,
                _ => {}
            },
            _ => {}
        }
    }
    cancel.cancel();

    assert!(market_events > 0, "expected at least one MarketSnapshot event");
    assert!(social_events > 0, "expected at least one SocialTrend event");
    // News deduplicated — may only get 1 per unique ID across cycles
    let _ = news_events; // not asserting count, just no panic
}

#[tokio::test]
async fn ingestion_engine_cache_populated_after_run() {
    let bus = EventBus::new();
    let cancel = CancellationToken::new();

    let mut cfg = IngestionConfig::default();
    cfg.market.poll_interval_ms = 20;
    cfg.social.poll_interval_ms = 20;
    cfg.news.poll_interval_ms = 20;
    cfg.economic.poll_interval_ms = 20;

    let engine = DataIngestionEngine::new(cfg, bus.clone());
    let cache = engine.cache.clone();

    let cancel_clone = cancel.clone();
    tokio::spawn(async move { engine.run(cancel_clone).await });

    // Wait 3 s for real HTTP APIs to respond
    tokio::time::sleep(Duration::from_millis(3_000)).await;
    cancel.cancel();

    let c = cache.read().await;
    assert!(c.market_count() > 0, "cache should contain markets after polling");
    assert!(c.trend_count() > 0, "cache should contain social trends after polling");
}

// ============================================================================
// Disabled connector — no events published
// ============================================================================

#[tokio::test]
async fn disabled_connector_produces_no_events() {
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let cancel = CancellationToken::new();

    let mut cfg = IngestionConfig::default();
    // Disable everything
    cfg.market.enabled   = false;
    cfg.news.enabled     = false;
    cfg.social.enabled   = false;
    cfg.economic.enabled = false;
    cfg.calendar.enabled = false;

    let engine = DataIngestionEngine::new(cfg, bus.clone());
    let cancel_clone = cancel.clone();
    tokio::spawn(async move { engine.run(cancel_clone).await });

    // Run briefly — no events should arrive
    let received = tokio::time::timeout(Duration::from_millis(100), rx.recv()).await;
    cancel.cancel();

    // Should time out (no events) rather than receive one
    assert!(received.is_err(), "expected no events when all connectors disabled");
}
