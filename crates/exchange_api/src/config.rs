// crates/exchange_api/src/config.rs
//
// Externalized configuration for exchange connectors.

/// Configuration for an exchange connector.
#[derive(Debug, Clone)]
pub struct ConnectorConfig {
    /// Base URL for the exchange API.
    pub base_url: String,
    /// Fee rate as a fraction (e.g. 0.02 = 2%).
    pub fee_rate: f64,
    /// Minimum order size in USD.
    pub min_order_usd: f64,
    /// Maximum number of retry attempts for transient errors.
    pub max_retries: u32,
    /// Base delay in milliseconds for exponential backoff.
    pub retry_base_delay_ms: u64,
}

impl ConnectorConfig {
    /// Default configuration for the Polymarket CLOB API.
    pub fn polymarket_defaults() -> Self {
        Self {
            base_url: "https://clob.polymarket.com".to_string(),
            fee_rate: 0.005,
            min_order_usd: 1.0,
            max_retries: 3,
            retry_base_delay_ms: 250,
        }
    }

    /// Default configuration for the Kalshi trading API.
    pub fn kalshi_defaults() -> Self {
        Self {
            base_url: "https://trading-api.kalshi.com/trade-api/v2".to_string(),
            fee_rate: 0.02,
            min_order_usd: 1.0,
            max_retries: 3,
            retry_base_delay_ms: 250,
        }
    }
}
