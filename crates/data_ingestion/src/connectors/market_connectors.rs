// crates/data_ingestion/src/connectors/market_connectors.rs
//
// MarketConnector trait + real HTTP implementations for Polymarket and Kalshi.

use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use std::time::Duration;

use super::{RawMarket, RawOrderBook, RawTrade};

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

#[async_trait]
pub trait MarketConnector: Send + Sync {
    /// Name used in logs and metrics labels.
    fn name(&self) -> &str;

    /// Fetch the current list of active markets.
    async fn fetch_markets(&self) -> Result<Vec<RawMarket>>;

    /// Fetch order book snapshots for each active market.
    async fn fetch_orderbooks(&self) -> Result<Vec<RawOrderBook>>;

    /// Fetch recent trades across all markets.
    async fn fetch_trades(&self) -> Result<Vec<RawTrade>>;

    /// Subscribe to a streaming update feed (returns None when unsupported).
    fn stream_url(&self) -> Option<String>;
}

// ---------------------------------------------------------------------------
// Polymarket Gamma API serde types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct PolymarketGammaMarket {
    id: String,
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(rename = "outcomePrices", default)]
    outcome_prices: Option<String>,
    #[serde(default)]
    volume: Option<String>,
    #[serde(default)]
    liquidity: Option<String>,
    #[serde(default)]
    active: bool,
    #[serde(default)]
    closed: bool,
}

// ---------------------------------------------------------------------------
// Polymarket connector
// ---------------------------------------------------------------------------

pub struct PolymarketIngestionConnector {
    client: reqwest::Client,
}

impl PolymarketIngestionConnector {
    pub fn new(timeout_ms: u64) -> anyhow::Result<Self> {
        let client = reqwest::ClientBuilder::new()
            .timeout(Duration::from_millis(timeout_ms))
            .user_agent("prediction-market-bot/1.0")
            .build()?;
        Ok(Self { client })
    }
}

impl Default for PolymarketIngestionConnector {
    fn default() -> Self {
        Self::new(10_000).expect("failed to build PolymarketIngestionConnector default client")
    }
}

#[async_trait]
impl MarketConnector for PolymarketIngestionConnector {
    fn name(&self) -> &str {
        "polymarket"
    }

    async fn fetch_markets(&self) -> Result<Vec<RawMarket>> {
        let url = "https://gamma-api.polymarket.com/markets?active=true&closed=false&limit=100&order=volume24hr&ascending=false";

        let resp = match self.client.get(url).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(connector = "polymarket", err = %e, "HTTP request failed");
                return Ok(vec![]);
            }
        };

        if !resp.status().is_success() {
            tracing::warn!(
                connector = "polymarket",
                status = %resp.status(),
                "HTTP error response"
            );
            return Ok(vec![]);
        }

        let raw: Vec<PolymarketGammaMarket> = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(connector = "polymarket", err = %e, "failed to parse response");
                return Ok(vec![]);
            }
        };

        let mut markets = vec![];
        for m in raw {
            if !m.active || m.closed {
                continue;
            }

            // Parse outcomePrices — it's a JSON-encoded string like "[\"0.52\",\"0.48\"]"
            let yes_price = match &m.outcome_prices {
                Some(s) => {
                    let prices: Vec<String> = match serde_json::from_str(s) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    match prices.first().and_then(|p| p.parse::<f64>().ok()) {
                        Some(v) => v,
                        None => continue,
                    }
                }
                None => continue,
            };

            if yes_price <= 0.01 || yes_price >= 0.99 {
                continue;
            }

            let bid = (yes_price - 0.01).max(0.01);
            let ask = (yes_price + 0.01).min(0.99);

            let volume = m
                .volume
                .as_deref()
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(0.0);
            let liquidity = m
                .liquidity
                .as_deref()
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(0.0);

            markets.push(RawMarket {
                market_id: m.id,
                title: m.question.unwrap_or_default(),
                description: m.description.unwrap_or_default(),
                probability: yes_price,
                bid,
                ask,
                volume,
                liquidity,
            });
        }

        tracing::info!(count = markets.len(), "polymarket: fetched {} markets", markets.len());
        Ok(markets)
    }

    async fn fetch_orderbooks(&self) -> Result<Vec<RawOrderBook>> {
        Ok(vec![])
    }

    async fn fetch_trades(&self) -> Result<Vec<RawTrade>> {
        Ok(vec![])
    }

    fn stream_url(&self) -> Option<String> {
        None
    }
}

// ---------------------------------------------------------------------------
// Kalshi API serde types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct KalshiMarketsResponse {
    markets: Vec<KalshiMarket>,
    #[serde(default)]
    cursor: String,
}

/// elections.kalshi.com v2 API returns prices as quoted-decimal strings ("0.4200"),
/// not integer cents.  Fields mirror the market_scanner KalshiMarket struct.
#[derive(Deserialize)]
struct KalshiMarket {
    ticker: String,
    #[serde(default)]
    status: String,
    /// YES bid as decimal string e.g. "0.4200".
    #[serde(default)]
    yes_bid_dollars: Option<String>,
    /// YES ask as decimal string e.g. "0.4400".
    #[serde(default)]
    yes_ask_dollars: Option<String>,
    /// Last trade price as decimal string.
    #[serde(default)]
    yes_last_price: Option<String>,
    /// Cumulative volume string.
    #[serde(default)]
    volume_fp: Option<String>,
    #[serde(default)]
    subtitle: Option<String>,
    #[serde(default)]
    rules_primary: Option<String>,
}

// ---------------------------------------------------------------------------
// Kalshi connector
// ---------------------------------------------------------------------------

pub struct KalshiIngestionConnector {
    client: reqwest::Client,
}

impl KalshiIngestionConnector {
    pub fn new(timeout_ms: u64) -> anyhow::Result<Self> {
        let client = reqwest::ClientBuilder::new()
            .timeout(Duration::from_millis(timeout_ms))
            .user_agent("prediction-market-bot/1.0")
            .build()?;
        Ok(Self { client })
    }
}

impl Default for KalshiIngestionConnector {
    fn default() -> Self {
        Self::new(10_000).expect("failed to build KalshiIngestionConnector default client")
    }
}

#[async_trait]
impl MarketConnector for KalshiIngestionConnector {
    fn name(&self) -> &str {
        "kalshi"
    }

    async fn fetch_markets(&self) -> Result<Vec<RawMarket>> {
        // elections.kalshi.com is the publicly accessible endpoint (no auth required).
        // trading-api.kalshi.com requires OAuth and returns 401 without credentials.
        let url = "https://api.elections.kalshi.com/trade-api/v2/markets?limit=100";

        let resp = match self.client.get(url).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(connector = "kalshi", err = %e, "HTTP request failed");
                return Ok(vec![]);
            }
        };

        if !resp.status().is_success() {
            tracing::warn!(
                connector = "kalshi",
                status = %resp.status(),
                "HTTP error response"
            );
            return Ok(vec![]);
        }

        let raw: KalshiMarketsResponse = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(connector = "kalshi", err = %e, "failed to parse response");
                return Ok(vec![]);
            }
        };

        let _ = raw.cursor; // may be used for pagination in the future

        let mut markets = vec![];
        for m in raw.markets {
            // Elections API uses "active"; trading API uses "open". Accept both.
            if m.status != "open" && m.status != "active" {
                continue;
            }

            // Parse decimal-string prices from elections API.
            let yes_bid: f64 = m.yes_bid_dollars.as_deref()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0);
            let yes_ask: f64 = m.yes_ask_dollars.as_deref()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0);
            let last_price: f64 = m.yes_last_price.as_deref()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0);

            // Use mid of bid/ask when available, otherwise last_price.
            let probability = if yes_bid > 0.0 || yes_ask > 0.0 {
                ((yes_bid + yes_ask) / 2.0).clamp(0.0, 1.0)
            } else {
                last_price.clamp(0.0, 1.0)
            };
            if probability <= 0.01 || probability >= 0.99 {
                continue;
            }

            let bid = if yes_bid > 0.0 { yes_bid } else { (probability - 0.01).max(0.01) };
            let ask = if yes_ask > 0.0 { yes_ask } else { (probability + 0.01).min(0.99) };

            let title = m.subtitle.clone().unwrap_or_else(|| m.ticker.clone());
            let description = m.rules_primary.unwrap_or_default();
            let volume = m.volume_fp.as_deref()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0);

            markets.push(RawMarket {
                market_id: m.ticker,
                title,
                description,
                probability,
                bid,
                ask,
                volume,
                liquidity: 0.0,
            });
        }

        tracing::info!(count = markets.len(), "kalshi: fetched {} markets", markets.len());
        Ok(markets)
    }

    async fn fetch_orderbooks(&self) -> Result<Vec<RawOrderBook>> {
        Ok(vec![])
    }

    async fn fetch_trades(&self) -> Result<Vec<RawTrade>> {
        Ok(vec![])
    }

    fn stream_url(&self) -> Option<String> {
        None
    }
}

// ---------------------------------------------------------------------------
// Mock connector (used in tests)
// ---------------------------------------------------------------------------

/// Configurable mock that returns preset markets, useful in unit tests.
pub struct MockMarketConnector {
    pub markets: Vec<RawMarket>,
}

impl MockMarketConnector {
    pub fn new(markets: Vec<RawMarket>) -> Self {
        Self { markets }
    }
}

#[async_trait]
impl MarketConnector for MockMarketConnector {
    fn name(&self) -> &str {
        "mock_market"
    }

    async fn fetch_markets(&self) -> Result<Vec<RawMarket>> {
        Ok(self.markets.clone())
    }

    async fn fetch_orderbooks(&self) -> Result<Vec<RawOrderBook>> {
        Ok(self
            .markets
            .iter()
            .map(|m| RawOrderBook {
                market_id: m.market_id.clone(),
                best_bid: m.bid,
                best_ask: m.ask,
                bid_depth: m.liquidity * 0.5,
                ask_depth: m.liquidity * 0.5,
            })
            .collect())
    }

    async fn fetch_trades(&self) -> Result<Vec<RawTrade>> {
        Ok(vec![])
    }

    fn stream_url(&self) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn polymarket_stub_returns_two_markets() {
        let c = PolymarketIngestionConnector::default();
        let markets = c.fetch_markets().await.unwrap();
        assert!(!markets.is_empty());
        for m in &markets {
            assert!(m.probability > 0.0 && m.probability < 1.0);
            assert!(m.bid < m.ask, "bid must be below ask");
        }
    }

    #[tokio::test]
    #[ignore]
    async fn kalshi_stub_returns_two_markets() {
        let c = KalshiIngestionConnector::default();
        let markets = c.fetch_markets().await.unwrap();
        assert!(!markets.is_empty());
    }

    #[tokio::test]
    async fn mock_connector_returns_preset_markets() {
        let preset = vec![RawMarket {
            market_id: "test_m".into(),
            title: "Test".into(),
            description: "desc".into(),
            probability: 0.5,
            bid: 0.49,
            ask: 0.51,
            volume: 1000.0,
            liquidity: 500.0,
        }];
        let c = MockMarketConnector::new(preset.clone());
        let markets = c.fetch_markets().await.unwrap();
        assert_eq!(markets.len(), 1);
        assert_eq!(markets[0].market_id, "test_m");
    }
}
