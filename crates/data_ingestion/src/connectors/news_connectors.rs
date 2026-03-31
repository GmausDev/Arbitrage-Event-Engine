// crates/data_ingestion/src/connectors/news_connectors.rs

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use serde::Deserialize;
use std::time::Duration;

use super::{fetch_with_retry, RawNewsItem};

#[async_trait]
pub trait NewsConnector: Send + Sync {
    fn name(&self) -> &str;
    async fn fetch_headlines(&self) -> Result<Vec<RawNewsItem>>;
    async fn fetch_articles(&self, ids: &[String]) -> Result<Vec<RawNewsItem>>;
}

// ---------------------------------------------------------------------------
// NewsAPI.org serde types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct NewsApiResponse {
    #[serde(default)]
    articles: Vec<NewsApiArticle>,
}

#[derive(Deserialize)]
struct NewsApiArticle {
    source: NewsApiSource,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(rename = "publishedAt", default)]
    published_at: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize)]
struct NewsApiSource {
    #[serde(default)]
    name: Option<String>,
}

// ---------------------------------------------------------------------------
// NewsAPI.org connector
// ---------------------------------------------------------------------------

pub struct NewsApiConnector {
    client: reqwest::Client,
    base_url: String,
    max_retries: u32,
    retry_base_delay_ms: u64,
}

impl NewsApiConnector {
    pub fn new(timeout_ms: u64) -> anyhow::Result<Self> {
        let client = reqwest::ClientBuilder::new()
            .timeout(Duration::from_millis(timeout_ms))
            .user_agent("prediction-market-bot/1.0")
            .build()?;
        Ok(Self {
            client,
            base_url: "https://newsapi.org".into(),
            max_retries: 3,
            retry_base_delay_ms: 500,
        })
    }

    pub fn with_config(timeout_ms: u64, base_url: String, max_retries: u32, retry_base_delay_ms: u64) -> anyhow::Result<Self> {
        let client = reqwest::ClientBuilder::new()
            .timeout(Duration::from_millis(timeout_ms))
            .user_agent("prediction-market-bot/1.0")
            .build()?;
        Ok(Self { client, base_url, max_retries, retry_base_delay_ms })
    }
}

impl Default for NewsApiConnector {
    fn default() -> Self {
        Self::new(10_000).unwrap_or_else(|e| {
            tracing::error!(err = %e, "NewsApiConnector: failed to build HTTP client, using no-op fallback");
            Self {
                client: reqwest::Client::new(),
                base_url: "https://newsapi.org".into(),
                max_retries: 3,
                retry_base_delay_ms: 500,
            }
        })
    }
}

#[async_trait]
impl NewsConnector for NewsApiConnector {
    fn name(&self) -> &str {
        "news_api"
    }

    async fn fetch_headlines(&self) -> Result<Vec<RawNewsItem>> {
        let key = match std::env::var("NEWS_API_KEY") {
            Ok(k) => k,
            Err(_) => {
                tracing::warn!(connector = "news_api", "NEWS_API_KEY not set; skipping fetch");
                return Ok(vec![]);
            }
        };

        let url = format!(
            "{}/v2/top-headlines?q=prediction%20market%20OR%20economy%20OR%20politics%20OR%20federal%20reserve&language=en&pageSize=20",
            self.base_url
        );

        let client = &self.client;
        let resp = match fetch_with_retry(
            || client.get(&url).header("X-Api-Key", &key),
            self.max_retries,
            self.retry_base_delay_ms,
            "news_api",
        ).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(connector = "news_api", err = %e, "HTTP request failed after retries");
                return Ok(vec![]);
            }
        };

        if !resp.status().is_success() {
            tracing::warn!(
                connector = "news_api",
                status = %resp.status(),
                "HTTP error response"
            );
            return Ok(vec![]);
        }

        let raw: NewsApiResponse = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(connector = "news_api", err = %e, "failed to parse response");
                return Ok(vec![]);
            }
        };

        let mut items = vec![];
        for article in raw.articles {
            let id = match &article.url {
                Some(u) if !u.is_empty() => u.clone(),
                _ => continue,
            };
            let headline = article.title.unwrap_or_default();
            if headline.is_empty() {
                continue;
            }
            let body = article
                .content
                .unwrap_or_else(|| article.description.unwrap_or_default());
            let source = article.source.name.unwrap_or_default();
            let published_at = article
                .published_at
                .as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(Utc::now);

            items.push(RawNewsItem {
                id,
                headline,
                body,
                source,
                entities: vec![],
                topics: vec!["news".into()],
                published_at,
            });
        }

        tracing::info!(count = items.len(), "news_api: fetched {} headlines", items.len());
        Ok(items)
    }

    async fn fetch_articles(&self, _ids: &[String]) -> Result<Vec<RawNewsItem>> {
        Ok(vec![])
    }
}

// ---------------------------------------------------------------------------
// AlphaVantage News serde types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct AlphaVantageResponse {
    #[serde(default)]
    feed: Vec<AlphaVantageArticle>,
}

#[derive(Deserialize)]
struct AlphaVantageArticle {
    title: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    time_published: String,
}

// ---------------------------------------------------------------------------
// AlphaVantage connector
// ---------------------------------------------------------------------------

pub struct AlphaVantageNewsConnector {
    client: reqwest::Client,
    base_url: String,
    max_retries: u32,
    retry_base_delay_ms: u64,
}

impl AlphaVantageNewsConnector {
    pub fn new(timeout_ms: u64) -> anyhow::Result<Self> {
        let client = reqwest::ClientBuilder::new()
            .timeout(Duration::from_millis(timeout_ms))
            .user_agent("prediction-market-bot/1.0")
            .build()?;
        Ok(Self {
            client,
            base_url: "https://www.alphavantage.co".into(),
            max_retries: 3,
            retry_base_delay_ms: 500,
        })
    }

    pub fn with_config(timeout_ms: u64, base_url: String, max_retries: u32, retry_base_delay_ms: u64) -> anyhow::Result<Self> {
        let client = reqwest::ClientBuilder::new()
            .timeout(Duration::from_millis(timeout_ms))
            .user_agent("prediction-market-bot/1.0")
            .build()?;
        Ok(Self { client, base_url, max_retries, retry_base_delay_ms })
    }
}

impl Default for AlphaVantageNewsConnector {
    fn default() -> Self {
        Self::new(10_000).unwrap_or_else(|e| {
            tracing::error!(err = %e, "AlphaVantageNewsConnector: failed to build HTTP client, using no-op fallback");
            Self {
                client: reqwest::Client::new(),
                base_url: "https://www.alphavantage.co".into(),
                max_retries: 3,
                retry_base_delay_ms: 500,
            }
        })
    }
}

#[async_trait]
impl NewsConnector for AlphaVantageNewsConnector {
    fn name(&self) -> &str {
        "alpha_vantage_news"
    }

    async fn fetch_headlines(&self) -> Result<Vec<RawNewsItem>> {
        let key = match std::env::var("ALPHA_VANTAGE_KEY") {
            Ok(k) => k,
            Err(_) => {
                tracing::warn!(connector = "alpha_vantage_news", "ALPHA_VANTAGE_KEY not set; skipping fetch");
                return Ok(vec![]);
            }
        };

        let url = reqwest::Url::parse_with_params(
            &format!("{}/query", self.base_url),
            &[
                ("function", "NEWS_SENTIMENT"),
                ("topics",   "economy_macro,financial_markets"),
                ("sort",     "LATEST"),
                ("limit",    "20"),
                ("apikey",   &key),
            ],
        ).map_err(|e| anyhow::anyhow!("bad URL: {e}"))?;

        let client = &self.client;
        let url_str = url.to_string();
        let resp = match fetch_with_retry(
            || client.get(&url_str),
            self.max_retries,
            self.retry_base_delay_ms,
            "alpha_vantage_news",
        ).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(connector = "alpha_vantage_news", err = %e, "HTTP request failed after retries");
                return Ok(vec![]);
            }
        };

        if !resp.status().is_success() {
            tracing::warn!(
                connector = "alpha_vantage_news",
                status = %resp.status(),
                "HTTP error response"
            );
            return Ok(vec![]);
        }

        let raw: AlphaVantageResponse = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(connector = "alpha_vantage_news", err = %e, "failed to parse response");
                return Ok(vec![]);
            }
        };

        let mut items = vec![];
        for article in raw.feed {
            let id = if article.url.is_empty() {
                continue;
            } else {
                article.url.clone()
            };
            let headline = article.title.clone();
            if headline.is_empty() {
                continue;
            }

            // AlphaVantage time format: "20240115T103000"
            let published_at = if article.time_published.is_empty() {
                Utc::now()
            } else {
                NaiveDateTime::parse_from_str(&article.time_published, "%Y%m%dT%H%M%S")
                    .map(|ndt| Utc.from_utc_datetime(&ndt))
                    .unwrap_or_else(|_| Utc::now())
            };

            items.push(RawNewsItem {
                id,
                headline,
                body: article.summary,
                source: article.source,
                entities: vec![],
                topics: vec!["news".into()],
                published_at,
            });
        }

        tracing::info!(count = items.len(), "alpha_vantage_news: fetched {} headlines", items.len());
        Ok(items)
    }

    async fn fetch_articles(&self, _ids: &[String]) -> Result<Vec<RawNewsItem>> {
        Ok(vec![])
    }
}

// ---------------------------------------------------------------------------
// Mock (tests)
// ---------------------------------------------------------------------------

pub struct MockNewsConnector {
    pub items: Vec<RawNewsItem>,
}

impl MockNewsConnector {
    pub fn new(items: Vec<RawNewsItem>) -> Self {
        Self { items }
    }

    pub fn empty() -> Self {
        Self { items: vec![] }
    }
}

#[async_trait]
impl NewsConnector for MockNewsConnector {
    fn name(&self) -> &str {
        "mock_news"
    }

    async fn fetch_headlines(&self) -> Result<Vec<RawNewsItem>> {
        Ok(self.items.clone())
    }

    async fn fetch_articles(&self, _ids: &[String]) -> Result<Vec<RawNewsItem>> {
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn news_api_stub_returns_one_item() {
        let c = NewsApiConnector::default();
        let items = c.fetch_headlines().await.unwrap();
        assert!(!items.is_empty());
        assert!(!items[0].headline.is_empty());
    }

    #[tokio::test]
    #[ignore]
    async fn alpha_vantage_returns_items() {
        let c = AlphaVantageNewsConnector::default();
        let items = c.fetch_headlines().await.unwrap();
        assert!(!items.is_empty());
    }

    #[tokio::test]
    async fn mock_news_connector_returns_preset() {
        let item = RawNewsItem {
            id: "t1".into(),
            headline: "Test headline".into(),
            body: "Body".into(),
            source: "Test".into(),
            entities: vec![],
            topics: vec!["test".into()],
            published_at: Utc::now(),
        };
        let c = MockNewsConnector::new(vec![item]);
        let result = c.fetch_headlines().await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "t1");
    }
}
