// crates/data_ingestion/src/connectors/mod.rs
//
// Raw API response types shared by all connectors, plus sub-module declarations.

pub mod calendar_connectors;
pub mod economic_connectors;
pub mod market_connectors;
pub mod news_connectors;
pub mod social_connectors;

use chrono::{DateTime, Utc};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Retry helper — exponential backoff for transient HTTP failures
// ---------------------------------------------------------------------------

/// Execute an async HTTP request with exponential backoff retry.
///
/// Retries on:
///   - Network / connection errors (reqwest::Error where `is_connect()`, `is_timeout()`, `is_request()`)
///   - HTTP 429 Too Many Requests
///   - HTTP 5xx server errors
///
/// Does NOT retry on:
///   - Successful responses (2xx, 3xx)
///   - Client errors (4xx) other than 429
pub(crate) async fn fetch_with_retry(
    request_builder_fn: impl Fn() -> reqwest::RequestBuilder,
    max_retries: u32,
    retry_base_delay_ms: u64,
    connector_name: &str,
) -> Result<reqwest::Response, reqwest::Error> {
    let mut attempt = 0u32;
    loop {
        let result = request_builder_fn().send().await;
        match result {
            Ok(resp) => {
                let status = resp.status();
                // Retry on 429 or 5xx
                if (status == reqwest::StatusCode::TOO_MANY_REQUESTS
                    || status.is_server_error())
                    && attempt < max_retries
                {
                    tracing::warn!(
                        connector = connector_name,
                        attempt = attempt + 1,
                        max_retries = max_retries,
                        status = %status,
                        "retryable HTTP status, backing off"
                    );
                    let delay = retry_base_delay_ms.saturating_mul(1u64 << attempt);
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                    attempt += 1;
                    continue;
                }
                return Ok(resp);
            }
            Err(e) => {
                // Retry on transient network errors
                let is_transient = e.is_connect() || e.is_timeout() || e.is_request();
                if is_transient && attempt < max_retries {
                    tracing::warn!(
                        connector = connector_name,
                        attempt = attempt + 1,
                        max_retries = max_retries,
                        err = %e,
                        "transient HTTP error, backing off"
                    );
                    let delay = retry_base_delay_ms.saturating_mul(1u64 << attempt);
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                    attempt += 1;
                    continue;
                }
                return Err(e);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Raw market data (pre-normalisation)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RawMarket {
    pub market_id: String,
    pub title: String,
    pub description: String,
    pub probability: f64,
    pub bid: f64,
    pub ask: f64,
    pub volume: f64,
    pub liquidity: f64,
}

#[derive(Debug, Clone)]
pub struct RawOrderBook {
    pub market_id: String,
    pub best_bid: f64,
    pub best_ask: f64,
    pub bid_depth: f64,
    pub ask_depth: f64,
}

#[derive(Debug, Clone)]
pub struct RawTrade {
    pub market_id: String,
    pub price: f64,
    pub size: f64,
    /// "buy" or "sell"
    pub side: String,
    pub timestamp: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Raw news data
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RawNewsItem {
    pub id: String,
    pub headline: String,
    pub body: String,
    pub source: String,
    pub entities: Vec<String>,
    pub topics: Vec<String>,
    pub published_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Raw social data
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RawSocialPost {
    pub topic: String,
    pub mention_count: u64,
    pub rolling_window_secs: u64,
    pub sentiment_score: f64,
    pub velocity: f64,
}

// ---------------------------------------------------------------------------
// Raw economic data
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RawEconomicData {
    pub indicator: String,
    pub value: f64,
    pub forecast: Option<f64>,
    pub previous: Option<f64>,
    pub release_time: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Raw calendar data
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RawCalendarEntry {
    pub event_id: String,
    pub title: String,
    pub scheduled_time: DateTime<Utc>,
    pub category: String,
    pub expected_impact: f64,
}
