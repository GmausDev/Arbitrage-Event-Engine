// agents/market_scanner/src/polymarket.rs
// Polymarket CLOB API client.
//
// API reference: https://docs.polymarket.com/#get-markets
//
// Wire format modelled on the v2 CLOB REST API:
//   GET /markets?status=active&limit=100&next_cursor=<cursor>
//
// Authentication: public endpoints do not require a key.  Authenticated
// endpoints (placing orders) are out of scope for this scanner.

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
struct PolymarketMarketsResponse {
    data: Vec<PolymarketMarket>,
    /// Opaque cursor for the next page; absent or empty string on the last page.
    next_cursor: Option<String>,
}

/// Outcome token in a Polymarket market (current API uses this for price).
#[derive(Debug, Deserialize)]
struct PolymarketToken {
    #[allow(dead_code)]
    outcome: Option<String>,
    /// Price in [0.0, 1.0] for this outcome.
    price: Option<f64>,
}

/// A single market entry from Polymarket's CLOB API.
///
/// The live API returns `active`, `closed`, and `tokens` (with `price` per outcome).
/// Older docs also mention `best_bid`, `best_ask`, `status` — we support both shapes.
#[derive(Debug, Deserialize)]
struct PolymarketMarket {
    condition_id: String,
    question: Option<String>,
    best_bid: Option<f64>,
    best_ask: Option<f64>,
    volume_24h: Option<f64>,
    status: Option<String>,
    /// Current API: market is active when true.
    #[serde(default)]
    active: bool,
    /// Current API: market is closed when true.
    #[serde(default)]
    closed: bool,
    /// Current API: outcome tokens with price; first token is typically YES.
    #[serde(default)]
    tokens: Vec<PolymarketToken>,
}

// ── Client ────────────────────────────────────────────────────────────────────

/// Polymarket CLOB REST API client.
///
/// Fetches all active markets, normalises bid/ask mid-prices into
/// `probability`, and returns [`MarketNode`] values ready for the Event Bus.
pub struct PolymarketClient {
    http: Client,
    base_url: String,
}

impl PolymarketClient {
    /// Construct a client pointing at `base_url` with the given HTTP timeout.
    ///
    /// In production set `base_url` to `"https://clob.polymarket.com"`.
    /// In tests, point at a mock server.
    pub fn new(base_url: impl Into<String>, timeout_ms: u64) -> anyhow::Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .user_agent("prediction-agent/0.1")
            .build()
            .context("build Polymarket HTTP client")?;

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
            .query(&[("status", "active"), ("limit", "100")]);

        if let Some(c) = cursor {
            req = req.query(&[("next_cursor", c)]);
        }

        let resp: PolymarketMarketsResponse = req
            .send()
            .await
            .context("Polymarket GET /markets — network")?
            .error_for_status()
            .context("Polymarket GET /markets — HTTP status")?
            .json()
            .await
            .context("Polymarket GET /markets — deserialise")?;

        let nodes = resp.data.into_iter().filter_map(Self::normalise).collect();
        Ok((nodes, resp.next_cursor))
    }

    /// Normalise a raw Polymarket market into a canonical [`MarketNode`].
    ///
    /// Accepts markets that are active (either status == "active" or active && !closed).
    /// Probability: from best_bid/best_ask mid if present, else from first token's price.
    fn normalise(m: PolymarketMarket) -> Option<MarketNode> {
        let is_active = m.status.as_deref() == Some("active")
            || (m.active && !m.closed);
        if !is_active {
            return None;
        }

        let probability = match (m.best_bid, m.best_ask) {
            (Some(bid), Some(ask)) => ((bid + ask) / 2.0).clamp(0.0, 1.0),
            _ => {
                // Current API: use first outcome token's price as YES probability.
                let p = m.tokens.first().and_then(|t| t.price)?;
                p.clamp(0.0, 1.0)
            }
        };

        let liquidity = m.volume_24h.unwrap_or(0.0);

        Some(MarketNode {
            id: m.condition_id,
            probability,
            liquidity,
            last_update: Utc::now(),
        })
    }
}

// ── MarketDataSource impl ─────────────────────────────────────────────────────

#[async_trait]
impl MarketDataSource for PolymarketClient {
    fn name(&self) -> &str {
        "polymarket"
    }

    /// Fetches all active Polymarket markets, paging through the cursor until
    /// the last page is reached.
    async fn fetch_markets(&self) -> anyhow::Result<Vec<MarketNode>> {
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;
        let mut pages = 0usize;

        loop {
            if pages >= MAX_PAGES {
                warn!(
                    "polymarket: reached page limit ({MAX_PAGES}), \
                     stopping pagination to avoid infinite loop"
                );
                break;
            }
            pages += 1;

            let (nodes, next) = self.fetch_page(cursor.as_deref()).await?;
            debug!(
                "polymarket: page {pages}/{MAX_PAGES} — {} markets, next_cursor={:?}",
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
