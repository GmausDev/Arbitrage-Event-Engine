// crates/data_ingestion/src/config.rs

use serde::{Deserialize, Serialize};

/// Per-connector configuration: polling interval, rate limit, retry policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorConfig {
    /// Whether this connector is active.
    pub enabled: bool,
    /// How often to poll (milliseconds).
    pub poll_interval_ms: u64,
    /// Maximum requests per second to this API.
    pub rate_limit_rps: f64,
    /// Per-request HTTP timeout (milliseconds).
    pub timeout_ms: u64,
    /// Maximum retries on transient failure before giving up for this cycle.
    pub max_retries: u32,
    /// Base delay for exponential backoff (milliseconds).
    pub retry_base_delay_ms: u64,
}

impl Default for ConnectorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval_ms: 5_000,
            rate_limit_rps: 2.0,
            timeout_ms: 10_000,
            max_retries: 3,
            retry_base_delay_ms: 500,
        }
    }
}

impl ConnectorConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.poll_interval_ms == 0 {
            return Err("poll_interval_ms must be > 0".into());
        }
        if self.rate_limit_rps <= 0.0 {
            return Err("rate_limit_rps must be > 0".into());
        }
        if self.timeout_ms == 0 {
            return Err("timeout_ms must be > 0".into());
        }
        Ok(())
    }
}

/// Top-level configuration for the entire `DataIngestionEngine`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestionConfig {
    /// Prediction market connector settings (Polymarket, Kalshi stubs).
    pub market: ConnectorConfig,
    /// News API connector settings.
    pub news: ConnectorConfig,
    /// Social media connector settings.
    pub social: ConnectorConfig,
    /// Economic data connector settings.
    pub economic: ConnectorConfig,
    /// Event calendar connector settings.
    pub calendar: ConnectorConfig,
    /// Delay between WebSocket reconnection attempts (milliseconds).
    pub stream_reconnect_delay_ms: u64,
    /// Maximum WebSocket reconnection attempts before giving up.
    pub stream_max_retries: u32,
    /// Maximum number of `NewsEvent` items to retain in the snapshot cache.
    pub cache_max_news_items: usize,
    /// Maximum number of `SocialTrend` topics to retain in the snapshot cache.
    pub cache_max_trend_items: usize,
}

impl Default for IngestionConfig {
    fn default() -> Self {
        Self {
            market: ConnectorConfig {
                poll_interval_ms: 2_000,
                rate_limit_rps: 5.0,
                ..ConnectorConfig::default()
            },
            news: ConnectorConfig {
                poll_interval_ms: 30_000,
                rate_limit_rps: 1.0,
                ..ConnectorConfig::default()
            },
            social: ConnectorConfig {
                poll_interval_ms: 10_000,
                rate_limit_rps: 2.0,
                ..ConnectorConfig::default()
            },
            economic: ConnectorConfig {
                poll_interval_ms: 60_000,
                rate_limit_rps: 0.5,
                ..ConnectorConfig::default()
            },
            calendar: ConnectorConfig {
                poll_interval_ms: 300_000,
                rate_limit_rps: 0.2,
                ..ConnectorConfig::default()
            },
            stream_reconnect_delay_ms: 2_000,
            stream_max_retries: 5,
            cache_max_news_items: 200,
            cache_max_trend_items: 50,
        }
    }
}

impl IngestionConfig {
    pub fn validate(&self) -> Result<(), String> {
        self.market.validate().map_err(|e| format!("market: {e}"))?;
        self.news.validate().map_err(|e| format!("news: {e}"))?;
        self.social.validate().map_err(|e| format!("social: {e}"))?;
        self.economic.validate().map_err(|e| format!("economic: {e}"))?;
        self.calendar.validate().map_err(|e| format!("calendar: {e}"))?;
        if self.cache_max_news_items == 0 {
            return Err("cache_max_news_items must be > 0".into());
        }
        if self.cache_max_trend_items == 0 {
            return Err("cache_max_trend_items must be > 0".into());
        }
        Ok(())
    }
}
