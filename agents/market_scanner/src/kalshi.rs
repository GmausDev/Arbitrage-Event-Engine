// agents/market_scanner/src/kalshi.rs
// Kalshi v2 REST API client.
//
// API reference: https://trading-api.kalshi.com/trade-api/v2
//
// Wire format modelled on the public `/markets` endpoint:
//   GET /trade-api/v2/markets?status=open&limit=100&cursor=<cursor>
//
// Public endpoints (market list, prices) do not require authentication.
// Placing orders requires JWT — out of scope for this scanner.

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
/// Prevents an infinite loop if the API returns a cursor that never terminates.
/// 50 pages × 100 markets/page = 5 000 markets maximum.
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
    title: Option<String>,

    /// Best YES bid in cents (0–99).
    yes_bid: Option<u32>,

    /// Best YES ask in cents (1–100).
    yes_ask: Option<u32>,

    /// Cumulative YES contract volume (number of contracts traded).
    volume: Option<f64>,

    /// Market status: `"open"` | `"closed"` | `"settled"`.
    status: Option<String>,
}

// ── Client ────────────────────────────────────────────────────────────────────

/// Kalshi v2 REST API client.
///
/// Fetches all open markets, normalises cent-unit bid/ask prices into
/// `probability` in [0, 1], and returns [`MarketNode`] values.
pub struct KalshiClient {
    http: Client,
    base_url: String,
}

impl KalshiClient {
    /// Construct a client pointing at `base_url` with the given HTTP timeout.
    ///
    /// In production set `base_url` to
    /// `"https://trading-api.kalshi.com/trade-api/v2"`.
    pub fn new(base_url: impl Into<String>, timeout_ms: u64) -> anyhow::Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .user_agent("prediction-agent/0.1")
            .build()
            .context("build Kalshi HTTP client")?;

        Ok(Self { http, base_url: base_url.into() })
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Fetch a single page of markets.  Returns `(nodes, next_cursor)`.
    async fn fetch_page(
        &self,
        cursor: Option<&str>,
    ) -> anyhow::Result<(Vec<MarketNode>, Option<String>)> {
        let mut req = self
            .http
            .get(format!("{}/markets", self.base_url))
            .query(&[("status", "open"), ("limit", "100")]);

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
    ///
    /// Returns `None` for:
    /// - Non-open markets (closed / settled)
    /// - Markets with no bid/ask quote
    fn normalise(m: KalshiMarket) -> Option<MarketNode> {
        if m.status.as_deref() != Some("open") {
            return None;
        }

        let yes_bid = m.yes_bid? as f64 / 100.0;
        let yes_ask = m.yes_ask? as f64 / 100.0;

        // Mid-price (cents → probability fraction).
        let probability = ((yes_bid + yes_ask) / 2.0).clamp(0.0, 1.0);
        let liquidity = m.volume.unwrap_or(0.0);

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

    /// Fetches all open Kalshi markets, paging through cursors until done.
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
