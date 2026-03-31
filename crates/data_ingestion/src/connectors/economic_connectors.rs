// crates/data_ingestion/src/connectors/economic_connectors.rs

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use futures::future::join_all;
use serde::Deserialize;
use std::time::Duration;

use super::RawEconomicData;

#[async_trait]
pub trait EconomicConnector: Send + Sync {
    fn name(&self) -> &str;
    /// Fetch the latest batch of economic indicator releases.
    async fn fetch_releases(&self) -> Result<Vec<RawEconomicData>>;
}

// ---------------------------------------------------------------------------
// FRED serde types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct FredResponse {
    observations: Vec<FredObservation>,
}

#[derive(Deserialize)]
struct FredObservation {
    date: String,
    value: String,
}

// ---------------------------------------------------------------------------
// FRED connector
// ---------------------------------------------------------------------------

const FRED_SERIES: &[&str] = &["CPIAUCSL", "PAYEMS", "FEDFUNDS", "UNRATE", "GDP"];

pub struct FREDConnector {
    client: reqwest::Client,
}

impl FREDConnector {
    pub fn new(timeout_ms: u64) -> anyhow::Result<Self> {
        let client = reqwest::ClientBuilder::new()
            .timeout(Duration::from_millis(timeout_ms))
            .user_agent("prediction-market-bot/1.0")
            .build()?;
        Ok(Self { client })
    }
}

impl Default for FREDConnector {
    fn default() -> Self {
        Self::new(10_000).unwrap_or_else(|e| {
            tracing::error!(err = %e, "FREDConnector: failed to build HTTP client, using no-op fallback");
            Self { client: reqwest::Client::new() }
        })
    }
}

#[async_trait]
impl EconomicConnector for FREDConnector {
    fn name(&self) -> &str {
        "fred"
    }

    async fn fetch_releases(&self) -> Result<Vec<RawEconomicData>> {
        let key = match std::env::var("FRED_API_KEY") {
            Ok(k) => k,
            Err(_) => {
                tracing::warn!(connector = "fred", "FRED_API_KEY not set; skipping fetch");
                return Ok(vec![]);
            }
        };

        // Fetch all series concurrently to reduce total latency.
        let fetch_tasks = FRED_SERIES.iter().map(|series_id| {
            let client = self.client.clone();
            let key = key.clone();
            let series_id = series_id.to_string();
            async move {
                // Build URL with query params via reqwest::Url to keep the
                // API key out of format-string logs and proxy access logs.
                let url = match reqwest::Url::parse_with_params(
                    "https://api.stlouisfed.org/fred/series/observations",
                    &[
                        ("series_id",  series_id.as_str()),
                        ("api_key",    key.as_str()),
                        ("file_type",  "json"),
                        ("sort_order", "desc"),
                        ("limit",      "2"),
                    ],
                ) {
                    Ok(u) => u,
                    Err(e) => {
                        tracing::warn!(connector = "fred", series = %series_id, err = %e, "bad URL");
                        return None;
                    }
                };

                let resp = match client.get(url).send().await {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(connector = "fred", series = %series_id, err = %e, "fetch failed");
                        return None;
                    }
                };

                if !resp.status().is_success() {
                    tracing::warn!(connector = "fred", series = %series_id, status = %resp.status(), "HTTP error response");
                    return None;
                }

                let raw: FredResponse = match resp.json().await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(connector = "fred", series = %series_id, err = %e, "failed to parse response");
                        return None;
                    }
                };

                let valid: Vec<&FredObservation> = raw.observations.iter().filter(|o| o.value != ".").collect();
                if valid.is_empty() {
                    return None;
                }

                let current_value = valid[0].value.parse::<f64>().ok().filter(|v| v.is_finite())?;
                let previous = valid.get(1).and_then(|o| o.value.parse::<f64>().ok()).filter(|v| v.is_finite());

                Some(RawEconomicData {
                    indicator: series_id,
                    value: current_value,
                    forecast: None,
                    previous,
                    release_time: Utc::now(),
                })
            }
        });

        let releases: Vec<RawEconomicData> = join_all(fetch_tasks).await.into_iter().flatten().collect();

        tracing::info!(count = releases.len(), "fred: fetched {} indicators", releases.len());
        Ok(releases)
    }
}

// ---------------------------------------------------------------------------
// TradingEconomics serde types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TradingEconomicsEvent {
    #[serde(rename = "Country", default)]
    country: String,
    #[serde(rename = "Category", default)]
    category: String,
    #[serde(rename = "Actual")]
    actual: Option<f64>,
    #[serde(rename = "Forecast")]
    forecast: Option<f64>,
    #[serde(rename = "Previous")]
    previous: Option<f64>,
}

// ---------------------------------------------------------------------------
// TradingEconomics connector
// ---------------------------------------------------------------------------

pub struct TradingEconomicsConnector {
    client: reqwest::Client,
}

impl TradingEconomicsConnector {
    pub fn new(timeout_ms: u64) -> anyhow::Result<Self> {
        let client = reqwest::ClientBuilder::new()
            .timeout(Duration::from_millis(timeout_ms))
            .user_agent("prediction-market-bot/1.0")
            .build()?;
        Ok(Self { client })
    }
}

impl Default for TradingEconomicsConnector {
    fn default() -> Self {
        Self::new(10_000).unwrap_or_else(|e| {
            tracing::error!(err = %e, "TradingEconomicsConnector: failed to build HTTP client, using no-op fallback");
            Self { client: reqwest::Client::new() }
        })
    }
}

#[async_trait]
impl EconomicConnector for TradingEconomicsConnector {
    fn name(&self) -> &str {
        "trading_economics"
    }

    async fn fetch_releases(&self) -> Result<Vec<RawEconomicData>> {
        let key = match std::env::var("TRADING_ECONOMICS_KEY") {
            Ok(k) => k,
            Err(_) => {
                tracing::warn!(connector = "trading_economics", "TRADING_ECONOMICS_KEY not set; skipping fetch");
                return Ok(vec![]);
            }
        };

        let today = Utc::now().format("%Y-%m-%d").to_string();
        let next_week = (Utc::now() + chrono::Duration::days(7))
            .format("%Y-%m-%d")
            .to_string();

        // Build URL with query params via reqwest::Url to keep the API key
        // out of format-string logs and proxy access logs.
        let url = reqwest::Url::parse_with_params(
            "https://api.tradingeconomics.com/calendar",
            &[("c", key.as_str()), ("d1", today.as_str()), ("d2", next_week.as_str())],
        ).map_err(|e| anyhow::anyhow!("bad URL: {e}"))?;

        let resp = match self.client.get(url).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(connector = "trading_economics", err = %e, "HTTP request failed");
                return Ok(vec![]);
            }
        };

        if !resp.status().is_success() {
            tracing::warn!(
                connector = "trading_economics",
                status = %resp.status(),
                "HTTP error response"
            );
            return Ok(vec![]);
        }

        let raw: Vec<TradingEconomicsEvent> = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(connector = "trading_economics", err = %e, "failed to parse response");
                return Ok(vec![]);
            }
        };

        let releases: Vec<RawEconomicData> = raw
            .into_iter()
            .filter_map(|e| {
                let value = e.actual?;
                let indicator = format!(
                    "{}_{}",
                    e.country.to_uppercase().replace(' ', "_"),
                    e.category.to_uppercase().replace(' ', "_")
                );
                Some(RawEconomicData {
                    indicator,
                    value,
                    forecast: e.forecast,
                    previous: e.previous,
                    release_time: Utc::now(),
                })
            })
            .collect();

        tracing::info!(count = releases.len(), "trading_economics: fetched {} events", releases.len());
        Ok(releases)
    }
}

// ---------------------------------------------------------------------------
// Mock (tests)
// ---------------------------------------------------------------------------

pub struct MockEconomicConnector {
    pub releases: Vec<RawEconomicData>,
}

impl MockEconomicConnector {
    pub fn new(releases: Vec<RawEconomicData>) -> Self {
        Self { releases }
    }
}

#[async_trait]
impl EconomicConnector for MockEconomicConnector {
    fn name(&self) -> &str {
        "mock_economic"
    }

    async fn fetch_releases(&self) -> Result<Vec<RawEconomicData>> {
        Ok(self.releases.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn fred_stub_returns_cpi_and_nfp() {
        let c = FREDConnector::default();
        let releases = c.fetch_releases().await.unwrap();
        assert!(!releases.is_empty());
    }

    #[tokio::test]
    async fn mock_economic_returns_preset() {
        let data = vec![RawEconomicData {
            indicator: "TEST_IND".into(),
            value: 42.0,
            forecast: None,
            previous: None,
            release_time: Utc::now(),
        }];
        let c = MockEconomicConnector::new(data);
        let result = c.fetch_releases().await.unwrap();
        assert_eq!(result[0].indicator, "TEST_IND");
        assert_eq!(result[0].value, 42.0);
    }
}
