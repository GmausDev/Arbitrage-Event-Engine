// agents/market_scanner/src/kalshi.rs
// Kalshi v2 REST API client.
//
// API reference: https://api.elections.kalshi.com/trade-api/v2
//
// Wire format: GET /markets?limit=100&cursor=<cursor>
// Returns { cursor, markets: [...] }.
// Prices are in "dollars" string format (e.g. "0.4200" = 42 cents probability).
//
// Public market endpoints do not require authentication.

use anyhow::Context;
use async_trait::async_trait;
use chrono::Utc;
use common::MarketNode;
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;
use tracing::{debug, warn};

use crate::source::MarketDataSource;

/// Maximum pages to fetch in a single `fetch_markets` call.
const MAX_PAGES: usize = 50;

// ── Wire-format types ─────────────────────────────────────────────────────────

/// Response body from `GET /markets`.
#[derive(Debug, Deserialize)]
struct KalshiMarketsResponse {
    markets: Vec<KalshiMarket>,
    /// Opaque pagination cursor; absent on the last page.
    cursor: Option<String>,
}

/// A single market entry from Kalshi's v2 API.
#[derive(Debug, Deserialize)]
struct KalshiMarket {
    /// Exchange ticker — used as our canonical `market_id`.
    ticker: String,

    /// Human-readable market title.
    #[allow(dead_code)]
    title: Option<String>,

    /// Best YES bid in dollars as a decimal string (e.g. "0.4200").
    yes_bid_dollars: Option<String>,

    /// Best YES ask in dollars as a decimal string (e.g. "0.4400").
    yes_ask_dollars: Option<String>,

    /// Cumulative volume in dollar-cents (string).
    volume_fp: Option<String>,

    /// Market status: `"active"` | `"closed"` | `"settled"`.
    status: Option<String>,
}

// ── Client ────────────────────────────────────────────────────────────────────

pub struct KalshiClient {
    http: Client,
    base_url: String,
}

impl KalshiClient {
    pub fn new(base_url: impl Into<String>, timeout_ms: u64) -> anyhow::Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .user_agent("prediction-agent/0.1")
            .build()
            .context("build Kalshi HTTP client")?;

        Ok(Self { http, base_url: base_url.into() })
    }

    async fn fetch_page(
        &self,
        cursor: Option<&str>,
    ) -> anyhow::Result<(Vec<MarketNode>, Option<String>)> {
        let mut req = self
            .http
            .get(format!("{}/markets", self.base_url))
            .query(&[("limit", "100")]);

        if let Some(c) = cursor {
            req = req.query(&[("cursor", c)]);
        }

        let resp: KalshiMarketsResponse = req
            .send()
            .await
            .context("Kalshi GET /markets — network")?
            .error_for_status()
            .context("Kalshi GET /markets — HTTP status")?
            .json()
            .await
            .context("Kalshi GET /markets — deserialise")?;

        let nodes = resp.markets.into_iter().filter_map(Self::normalise).collect();
        Ok((nodes, resp.cursor))
    }

    /// Normalise a raw Kalshi market into a canonical [`MarketNode`].
    fn normalise(m: KalshiMarket) -> Option<MarketNode> {
        // Only active markets (not closed/settled).
        if m.status.as_deref() != Some("active") {
            return None;
        }

        // Parse dollar-string prices → probability in [0, 1].
        let yes_bid: f64 = m.yes_bid_dollars.as_deref()?.parse().ok()?;
        let yes_ask: f64 = m.yes_ask_dollars.as_deref()?.parse().ok()?;

        // Skip markets with no live quote.
        if yes_bid <= 0.0 && yes_ask <= 0.0 {
            return None;
        }

        let probability = ((yes_bid + yes_ask) / 2.0).clamp(0.0, 1.0);

        // volume_fp is a string like "123456.78".
        let liquidity = m.volume_fp
            .as_deref()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);

        Some(MarketNode {
            id: m.ticker,
            probability,
            liquidity,
            last_update: Utc::now(),
        })
    }
}

// ── MarketDataSource impl ─────────────────────────────────────────────────────

#[async_trait]
impl MarketDataSource for KalshiClient {
    fn name(&self) -> &str {
        "kalshi"
    }

    async fn fetch_markets(&self) -> anyhow::Result<Vec<MarketNode>> {
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;
        let mut pages = 0usize;

        loop {
            if pages >= MAX_PAGES {
                warn!(
                    "kalshi: reached page limit ({MAX_PAGES}), \
                     stopping pagination to avoid infinite loop"
                );
                break;
            }
            pages += 1;

            let (nodes, next) = self.fetch_page(cursor.as_deref()).await?;
            debug!(
                "kalshi: page {pages}/{MAX_PAGES} — {} markets, cursor={:?}",
                nodes.len(),
                next
            );
            all.extend(nodes);

            match next {
                Some(c) if !c.is_empty() => cursor = Some(c),
                _ => break,
            }
        }

        Ok(all)
    }
}
