// agents/market_scanner/src/polymarket.rs
// Polymarket Gamma API client.
//
// API reference: https://gamma-api.polymarket.com
//
// Wire format: GET /markets?closed=false&active=true&limit=100&offset=N
// Returns a JSON array of markets directly (no wrapper object).
// Pagination via `offset` integer.
//
// Authentication: public endpoints do not require a key.

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
/// 50 pages × 100 markets/page = 5 000 markets maximum.
const MAX_PAGES: usize = 50;
const PAGE_SIZE: usize = 100;

// ── Wire-format types ─────────────────────────────────────────────────────────

/// A single market entry from Polymarket's Gamma API.
#[derive(Debug, Deserialize)]
struct PolymarketMarket {
    /// Hex condition ID — used as canonical market_id.
    #[serde(rename = "conditionId")]
    condition_id: String,

    /// Human-readable question text.
    question: Option<String>,

    /// Best bid price in [0.0, 1.0].
    #[serde(rename = "bestBid")]
    best_bid: Option<f64>,

    /// Best ask price in [0.0, 1.0].
    #[serde(rename = "bestAsk")]
    best_ask: Option<f64>,

    /// 24-hour trading volume in USD.
    volume: Option<f64>,

    /// Market is currently active (open for trading).
    #[serde(default)]
    active: bool,

    /// Market is closed / resolved.
    #[serde(default)]
    closed: bool,

    /// Outcome token prices; first token is typically YES.
    #[serde(default)]
    #[allow(dead_code)]
    outcomes: Vec<String>,

    /// Outcome token prices as strings (parallel array to `outcomes`).
    #[serde(rename = "outcomePrices", default)]
    outcome_prices: Vec<String>,
}

// ── Client ────────────────────────────────────────────────────────────────────

/// Polymarket Gamma REST API client.
pub struct PolymarketClient {
    http: Client,
    base_url: String,
}

impl PolymarketClient {
    pub fn new(base_url: impl Into<String>, timeout_ms: u64) -> anyhow::Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .user_agent("prediction-agent/0.1")
            .build()
            .context("build Polymarket HTTP client")?;

        Ok(Self { http, base_url: base_url.into() })
    }

    /// Fetch one page of markets at `offset`.  Returns the list of nodes.
    async fn fetch_page(&self, offset: usize) -> anyhow::Result<Vec<MarketNode>> {
        let resp: Vec<PolymarketMarket> = self
            .http
            .get(format!("{}/markets", self.base_url))
            .query(&[
                ("closed",  "false"),
                ("active",  "true"),
                ("limit",   &PAGE_SIZE.to_string()),
                ("offset",  &offset.to_string()),
            ])
            .send()
            .await
            .context("Polymarket GET /markets — network")?
            .error_for_status()
            .context("Polymarket GET /markets — HTTP status")?
            .json()
            .await
            .context("Polymarket GET /markets — deserialise")?;

        Ok(resp.into_iter().filter_map(Self::normalise).collect())
    }

    /// Normalise a raw Gamma market into a canonical [`MarketNode`].
    fn normalise(m: PolymarketMarket) -> Option<MarketNode> {
        if !m.active || m.closed {
            return None;
        }
        if m.condition_id.is_empty() {
            return None;
        }

        let probability = match (m.best_bid, m.best_ask) {
            (Some(bid), Some(ask)) if bid > 0.0 || ask > 0.0 => {
                ((bid + ask) / 2.0).clamp(0.0, 1.0)
            }
            _ => {
                // Fall back to first outcome price (YES token).
                let p: f64 = m.outcome_prices.first()?.parse().ok()?;
                p.clamp(0.0, 1.0)
            }
        };

        let liquidity = m.volume.unwrap_or(0.0);

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

    async fn fetch_markets(&self) -> anyhow::Result<Vec<MarketNode>> {
        let mut all = Vec::new();
        let mut offset = 0usize;

        for page in 0..MAX_PAGES {
            let nodes = self.fetch_page(offset).await?;
            let count = nodes.len();
            debug!(
                "polymarket: page {}/{MAX_PAGES} (offset={offset}) — {count} active markets",
                page + 1
            );
            all.extend(nodes);

            if count < PAGE_SIZE {
                break; // last page
            }
            offset += PAGE_SIZE;
        }

        if offset / PAGE_SIZE >= MAX_PAGES {
            warn!(
                "polymarket: reached page limit ({MAX_PAGES}), stopping pagination"
            );
        }

        Ok(all)
    }
}
