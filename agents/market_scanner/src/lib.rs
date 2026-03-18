// agents/market_scanner/src/lib.rs
// Market Scanner — Data Ingestion Layer
//
// ┌─────────────────────────────────────────────────────────────────────────┐
// │  Crate layout                                                           │
// │                                                                         │
// │  config.rs      — ScannerConfig (poll_interval, batch_size, …)         │
// │  source.rs      — MarketDataSource trait (async-trait)                 │
// │  polymarket.rs  — PolymarketClient (CLOB REST v2)                      │
// │  kalshi.rs      — KalshiClient (v2 REST)                               │
// │  scanner.rs     — MarketScanner + SharedScanner + ScannerState         │
// │  lib.rs         — public re-exports + unit tests                       │
// └─────────────────────────────────────────────────────────────────────────┘
//
// Subscribes to: nothing  (this is a source agent)
// Publishes:     Event::Market(MarketUpdate)
//
// # Quick-start
//
// ```rust,no_run
// use common::EventBus;
// use market_scanner::{MarketScanner, ScannerConfig, PolymarketClient, KalshiClient};
// use tokio_util::sync::CancellationToken;
//
// #[tokio::main]
// async fn main() {
//     let bus    = EventBus::new();
//     let config = ScannerConfig::default();
//     let cancel = CancellationToken::new();
//
//     let scanner = MarketScanner::new(bus.clone(), config.clone())
//         .with_source(Box::new(
//             PolymarketClient::new(&config.polymarket_base_url,
//                                   config.request_timeout_ms).unwrap(),
//         ))
//         .with_source(Box::new(
//             KalshiClient::new(&config.kalshi_base_url,
//                               config.request_timeout_ms).unwrap(),
//         ));
//
//     let state = scanner.state_handle();
//     let mut rx = bus.subscribe();
//
//     tokio::spawn(scanner.run(cancel.child_token()));
//
//     while let Ok(ev) = rx.recv().await {
//         if let common::Event::Market(update) = ev.as_ref() {
//             println!("market update: {:?}", update.market);
//         }
//     }
// }
// ```

pub mod config;
pub mod kalshi;
pub mod polymarket;
pub mod scanner;
pub mod source;

// ── Flat re-exports ───────────────────────────────────────────────────────────
pub use config::ScannerConfig;
pub use kalshi::KalshiClient;
pub use polymarket::PolymarketClient;
pub use scanner::{MarketScanner, ScannerState, SharedScanner};
pub use source::MarketDataSource;

// ── Unit tests ────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::Duration;

    use async_trait::async_trait;
    use chrono::Utc;
    use common::{Event, EventBus, MarketNode};
    use tokio_util::sync::CancellationToken;

    use super::*;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn node(id: &str, prob: f64) -> MarketNode {
        MarketNode {
            id: id.into(),
            probability: prob,
            liquidity: 1_000.0,
            last_update: Utc::now(),
        }
    }

    fn fast_config() -> ScannerConfig {
        ScannerConfig { poll_interval_ms: 50, ..Default::default() }
    }

    // ── Mock sources ──────────────────────────────────────────────────────────

    struct MockSource {
        name:    &'static str,
        markets: Vec<MarketNode>,
    }

    impl MockSource {
        fn new(name: &'static str, markets: Vec<MarketNode>) -> Box<Self> {
            Box::new(Self { name, markets })
        }
    }

    #[async_trait]
    impl MarketDataSource for MockSource {
        fn name(&self) -> &str { self.name }

        async fn fetch_markets(&self) -> anyhow::Result<Vec<MarketNode>> {
            Ok(self.markets.clone())
        }
    }

    /// A source that always returns an error.
    struct AlwaysErrorSource;

    #[async_trait]
    impl MarketDataSource for AlwaysErrorSource {
        fn name(&self) -> &str { "always_error" }

        async fn fetch_markets(&self) -> anyhow::Result<Vec<MarketNode>> {
            anyhow::bail!("simulated API failure")
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    /// A single-market source delivers exactly one Event::Market on the bus.
    #[tokio::test]
    async fn test_single_market_update_received() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let cancel = CancellationToken::new();

        let scanner = MarketScanner::new(bus.clone(), fast_config())
            .with_source(MockSource::new("mock", vec![node("MKT-A", 0.6)]));

        tokio::spawn(scanner.run(cancel.child_token()));

        let ev = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timed out waiting for MarketUpdate")
            .expect("recv error");

        assert!(matches!(ev.as_ref(), Event::Market(_)));
        if let Event::Market(u) = ev.as_ref() {
            assert_eq!(u.market.id, "MKT-A");
            assert!((u.market.probability - 0.6).abs() < f64::EPSILON);
        }
    }

    /// A source with multiple markets produces one event per market.
    #[tokio::test]
    async fn test_batch_update_publishes_all_markets() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let cancel = CancellationToken::new();

        let markets = vec![node("MKT-1", 0.3), node("MKT-2", 0.5), node("MKT-3", 0.7)];

        let scanner = MarketScanner::new(bus.clone(), fast_config())
            .with_source(MockSource::new("mock", markets));

        tokio::spawn(scanner.run(cancel.child_token()));

        let mut ids = HashSet::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while ids.len() < 3 && tokio::time::Instant::now() < deadline {
            if let Ok(Ok(ev)) =
                tokio::time::timeout(Duration::from_millis(500), rx.recv()).await
            {
                if let Event::Market(u) = ev.as_ref() {
                    ids.insert(u.market.id.clone());
                }
            }
        }

        assert_eq!(ids.len(), 3);
        assert!(ids.contains("MKT-1"));
        assert!(ids.contains("MKT-2"));
        assert!(ids.contains("MKT-3"));
    }

    /// Two sources are polled concurrently; both batches are delivered.
    #[tokio::test]
    async fn test_multiple_sources_merged() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let cancel = CancellationToken::new();

        let scanner = MarketScanner::new(bus.clone(), fast_config())
            .with_source(MockSource::new("source_a", vec![node("A-1", 0.4)]))
            .with_source(MockSource::new("source_b", vec![node("B-1", 0.6)]));

        tokio::spawn(scanner.run(cancel.child_token()));

        let mut ids = HashSet::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while ids.len() < 2 && tokio::time::Instant::now() < deadline {
            if let Ok(Ok(ev)) =
                tokio::time::timeout(Duration::from_millis(500), rx.recv()).await
            {
                if let Event::Market(u) = ev.as_ref() {
                    ids.insert(u.market.id.clone());
                }
            }
        }

        assert!(ids.contains("A-1"), "A-1 missing from {:?}", ids);
        assert!(ids.contains("B-1"), "B-1 missing from {:?}", ids);
    }

    /// A permanently failing source does not crash the scanner; the healthy
    /// source alongside it continues to deliver events.
    #[tokio::test]
    async fn test_error_source_does_not_crash_scanner() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let cancel = CancellationToken::new();

        let scanner = MarketScanner::new(
            bus.clone(),
            ScannerConfig { poll_interval_ms: 50, max_retries: 1, ..Default::default() },
        )
        .with_source(Box::new(AlwaysErrorSource))
        .with_source(MockSource::new("good", vec![node("GOOD-1", 0.5)]));

        tokio::spawn(scanner.run(cancel.child_token()));

        let ev = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout")
            .expect("recv error");

        assert!(matches!(ev.as_ref(), Event::Market(_)));
        if let Event::Market(u) = ev.as_ref() {
            assert_eq!(u.market.id, "GOOD-1");
        }
    }

    /// The SharedScanner state handle reflects completed ticks and market counts.
    #[tokio::test]
    async fn test_state_handle_tracks_ticks_and_markets() {
        let bus = EventBus::new();
        let cancel = CancellationToken::new();

        let scanner = MarketScanner::new(bus.clone(), fast_config())
            .with_source(MockSource::new("mock", vec![node("X", 0.5)]));

        let state = scanner.state_handle();
        tokio::spawn(scanner.run(cancel.child_token()));

        // Allow a few ticks to elapse.
        tokio::time::sleep(Duration::from_millis(250)).await;

        let s = state.read().await;
        assert!(s.ticks_completed >= 1, "expected ≥1 tick, got {}", s.ticks_completed);
        assert!(s.markets_seen   >= 1, "expected ≥1 market, got {}", s.markets_seen);
    }

    /// Markets exceeding `batch_size` are truncated before publishing.
    #[tokio::test]
    async fn test_batch_size_cap_is_enforced() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let cancel = CancellationToken::new();

        // 10 markets, cap at 3.
        let markets: Vec<_> = (0..10).map(|i| node(&format!("M{i}"), 0.5)).collect();

        let scanner = MarketScanner::new(
            bus.clone(),
            ScannerConfig { poll_interval_ms: 50, batch_size: 3, ..Default::default() },
        )
        .with_source(MockSource::new("mock", markets));

        tokio::spawn(scanner.run(cancel.child_token()));

        // Drain events for one tick window (50 ms poll + headroom).
        let mut count = 0usize;
        let window_end = tokio::time::Instant::now() + Duration::from_millis(120);
        while tokio::time::Instant::now() < window_end {
            match tokio::time::timeout(Duration::from_millis(30), rx.recv()).await {
                Ok(Ok(ev)) if matches!(ev.as_ref(), Event::Market(_)) => count += 1,
                _ => break,
            }
        }

        assert!(count <= 3, "expected ≤3 events per tick, got {count}");
    }
}
