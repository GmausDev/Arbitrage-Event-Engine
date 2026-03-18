// agents/market_scanner/src/scanner.rs
// MarketScanner — the main polling loop.
//
// Owns a set of MarketDataSource implementations.  On each tick it fans out
// concurrent fetches to all sources, merges the results, enforces the
// configured batch-size limit, then publishes one Event::Market per node to
// the Event Bus.
//
// Observable state (ticks completed, markets seen, …) is exposed behind a
// SharedScanner handle so other tasks can read it without taking a write lock.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use common::{Event, EventBus, MarketNode, MarketUpdate};
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use tokio::sync::RwLock;
use tokio::time::{self, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::config::ScannerConfig;
use crate::source::MarketDataSource;

// ── Observable state ──────────────────────────────────────────────────────────

/// Live statistics exposed by [`MarketScanner::state_handle`].
///
/// All fields are updated after every tick while holding the write lock, so
/// readers always see a consistent snapshot.
#[derive(Debug, Default)]
pub struct ScannerState {
    /// Total markets published since startup.
    pub markets_seen: usize,
    /// Number of tick intervals elapsed since startup.
    pub ticks_completed: u64,
    /// Number of markets published in the most recent tick.
    pub last_tick_market_count: usize,
}

/// Cheaply-cloneable read/write handle to the scanner's observable state.
pub type SharedScanner = Arc<RwLock<ScannerState>>;

// ── Internal future type alias ────────────────────────────────────────────────

/// Boxed, Send future that resolves to a batch of market nodes.
/// Used to make all per-source futures homogeneous for `FuturesUnordered`.
type SourceFut<'a> = Pin<Box<dyn Future<Output = Vec<MarketNode>> + Send + 'a>>;

// ── MarketScanner ─────────────────────────────────────────────────────────────

/// Polls all registered [`MarketDataSource`]s on each tick, normalises
/// results into [`common::MarketNode`] values, and publishes batched
/// [`Event::Market`] events to the Event Bus.
///
/// # Usage
/// ```rust,no_run
/// # use common::EventBus;
/// # use market_scanner::{MarketScanner, ScannerConfig, PolymarketClient, KalshiClient};
/// # use tokio_util::sync::CancellationToken;
/// #[tokio::main]
/// async fn main() {
///     let bus    = EventBus::new();
///     let config = ScannerConfig::default();
///     let cancel = CancellationToken::new();
///
///     let scanner = MarketScanner::new(bus.clone(), config.clone())
///         .with_source(Box::new(
///             PolymarketClient::new(&config.polymarket_base_url, config.request_timeout_ms).unwrap(),
///         ))
///         .with_source(Box::new(
///             KalshiClient::new(&config.kalshi_base_url, config.request_timeout_ms).unwrap(),
///         ));
///
///     // Grab the state handle before consuming the scanner.
///     let state = scanner.state_handle();
///
///     tokio::spawn(scanner.run(cancel.child_token()));
///
///     // … later …
///     cancel.cancel();
/// }
/// ```
pub struct MarketScanner {
    bus:     EventBus,
    sources: Vec<Box<dyn MarketDataSource>>,
    config:  ScannerConfig,
    state:   SharedScanner,
}

impl MarketScanner {
    /// Create a new scanner with no sources attached.
    /// Use [`with_source`](Self::with_source) to register data providers.
    pub fn new(bus: EventBus, config: ScannerConfig) -> Self {
        Self {
            bus,
            sources: Vec::new(),
            config,
            state: Arc::new(RwLock::new(ScannerState::default())),
        }
    }

    /// Builder: attach a single data source.
    pub fn with_source(mut self, source: Box<dyn MarketDataSource>) -> Self {
        self.sources.push(source);
        self
    }

    /// Builder: attach multiple sources at once.
    pub fn with_sources(mut self, sources: Vec<Box<dyn MarketDataSource>>) -> Self {
        self.sources.extend(sources);
        self
    }

    /// Returns a cloneable handle to the scanner's live statistics.
    ///
    /// Clone this handle before calling [`run`](Self::run) (which consumes
    /// `self`).  Other tasks can then read [`ScannerState`] without blocking
    /// the polling loop.
    pub fn state_handle(&self) -> SharedScanner {
        Arc::clone(&self.state)
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Fan out `fetch_markets` to all registered sources concurrently.
    ///
    /// Each source is retried up to `max_retries` times with exponential
    /// back-off on transient errors.  A source that exhausts its retries
    /// contributes an empty slice — the loop never panics or stalls.
    async fn poll_all(&self) -> Vec<MarketNode> {
        let max_retries = self.config.max_retries;

        // Build a set of boxed, concurrent futures — one per source.
        let mut tasks: FuturesUnordered<SourceFut<'_>> = FuturesUnordered::new();

        for src in &self.sources {
            let name = src.name().to_owned();
            let fut: SourceFut<'_> = Box::pin(async move {
                Self::fetch_with_retry(src.as_ref(), &name, max_retries).await
            });
            tasks.push(fut);
        }

        // Collect results as each source completes (true concurrency).
        let mut all = Vec::new();
        while let Some(markets) = tasks.next().await {
            all.extend(markets);
        }
        all
    }

    /// Attempt `fetch_markets` up to `1 + max_retries` times with exponential
    /// back-off between attempts (100 ms × 2^attempt).
    ///
    /// Returns an empty vec only after all attempts are exhausted.
    async fn fetch_with_retry(
        src: &dyn MarketDataSource,
        name: &str,
        max_retries: u32,
    ) -> Vec<MarketNode> {
        let mut attempt = 0u32;
        loop {
            match src.fetch_markets().await {
                Ok(markets) => return markets,
                Err(e) if attempt < max_retries => {
                    attempt += 1;
                    // Exponential back-off: 100ms, 200ms, 400ms, …
                    let delay = Duration::from_millis(100 * 2u64.pow(attempt - 1));
                    warn!(
                        "market_scanner: {name} fetch error \
                         (attempt {attempt}/{max_retries}, retrying in {delay:?}): {e:#}"
                    );
                    tokio::time::sleep(delay).await;
                }
                Err(e) => {
                    error!(
                        "market_scanner: {name} giving up after \
                         {max_retries} retries — {e:#}"
                    );
                    return vec![];
                }
            }
        }
    }

    /// Publish each market node as a separate `Event::Market` on the bus.
    ///
    /// `SendError` is expected during startup (no subscribers yet) and is
    /// silently ignored with a `warn!` rather than a panic.
    fn publish_batch(&self, markets: &[MarketNode]) {
        for market in markets {
            let event = Event::Market(MarketUpdate { market: market.clone() });
            if let Err(e) = self.bus.publish(event) {
                // Fired when there are zero active receivers — safe to ignore.
                warn!("market_scanner: publish error (no receivers?): {e}");
            }
        }

        // Prometheus counter: markets_scanned_total{source="all"}
        metrics::counter!("markets_scanned_total").increment(markets.len() as u64);
    }

    // ── Main loop ─────────────────────────────────────────────────────────────

    /// Start the polling loop.
    ///
    /// Runs until `cancel` is cancelled — use `tokio::spawn(scanner.run(token))`
    /// so it does not block the caller.  For clean shutdown:
    ///
    /// ```rust,no_run
    /// # use tokio_util::sync::CancellationToken;
    /// # let cancel = CancellationToken::new();
    /// // … spawn scanner …
    /// cancel.cancel(); // signals the scanner to exit after the current tick
    /// ```
    pub async fn run(self, cancel: CancellationToken) {
        let poll_ms   = self.config.poll_interval_ms;
        let batch_cap = self.config.batch_size;

        let mut interval = time::interval(Duration::from_millis(poll_ms));
        // Skip catch-up ticks after a slow fetch; don't hammer the APIs.
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        info!(
            "market_scanner: started — {} source(s), \
             poll_interval={}ms, batch_size={}",
            self.sources.len(),
            poll_ms,
            batch_cap,
        );

        loop {
            // Wait for the next tick or a cancellation signal, whichever comes first.
            tokio::select! {
                _ = cancel.cancelled() => {
                    info!("market_scanner: shutdown requested, exiting");
                    break;
                }
                _ = interval.tick() => {}
            }

            // Re-check before doing potentially slow fetch work.
            if cancel.is_cancelled() {
                break;
            }

            let mut markets = self.poll_all().await;

            if markets.len() > batch_cap {
                warn!(
                    "market_scanner: {} markets fetched, truncating to \
                     batch_size={}",
                    markets.len(),
                    batch_cap,
                );
                markets.truncate(batch_cap);
            }

            let count = markets.len();

            metrics::counter!("market_scanner_ticks_total").increment(1);
            if count > 0 {
                self.publish_batch(&markets);
                info!("market_scanner: published {count} MarketUpdate events this tick");
            } else {
                info!(
                    "market_scanner: poll returned 0 markets this tick (APIs may be empty, \
                     rate-limited, or response format changed — run with RUST_LOG=market_scanner=debug to inspect)"
                );
            }

            // Update observable state.
            {
                let mut s = self.state.write().await;
                s.markets_seen         += count;
                s.ticks_completed      += 1;
                s.last_tick_market_count = count;
            }
        }
    }
}
