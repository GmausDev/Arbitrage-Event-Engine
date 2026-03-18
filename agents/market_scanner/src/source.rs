// agents/market_scanner/src/source.rs
// MarketDataSource — the async trait all API clients implement.
//
// Add a new exchange by implementing this trait and registering the client
// with `MarketScanner::with_source(Box::new(MyClient::new(...)))`.

use async_trait::async_trait;
use common::MarketNode;

/// Abstraction over a single prediction-market data provider.
///
/// Each implementation is responsible for:
///   1. Authenticating with its API (if required)
///   2. Fetching the raw market list
///   3. Normalising raw data into canonical [`MarketNode`] values
///
/// The scanner calls [`fetch_markets`] on every tick and merges the results
/// from all registered sources before publishing to the Event Bus.
///
/// # Implementing a new source
/// ```rust,no_run
/// use async_trait::async_trait;
/// use common::MarketNode;
/// use market_scanner::MarketDataSource;
///
/// struct MyExchange;
///
/// #[async_trait]
/// impl MarketDataSource for MyExchange {
///     fn name(&self) -> &str { "my_exchange" }
///
///     async fn fetch_markets(&self) -> anyhow::Result<Vec<MarketNode>> {
///         // ... HTTP call + normalisation ...
///         Ok(vec![])
///     }
/// }
/// ```
#[async_trait]
pub trait MarketDataSource: Send + Sync {
    /// Short, lowercase identifier used in log messages and metric labels.
    /// Should be stable across restarts (e.g. `"polymarket"`, `"kalshi"`).
    fn name(&self) -> &str;

    /// Fetch and normalise the current market list from this source.
    ///
    /// Returns `Ok(vec![])` when there is nothing new to report.
    /// Returns `Err(…)` on transient failures — the scanner will retry up
    /// to `config.max_retries` times before skipping the source for this tick.
    async fn fetch_markets(&self) -> anyhow::Result<Vec<MarketNode>>;
}
