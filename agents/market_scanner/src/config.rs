// agents/market_scanner/src/config.rs
// Runtime configuration for the Market Scanner agent.

use serde::{Deserialize, Serialize};

/// Configuration for the [`MarketScanner`](crate::MarketScanner).
///
/// Can be loaded from TOML via the `[scanner]` table, or constructed
/// programmatically. All fields have sensible defaults.
///
/// # TOML example
/// ```toml
/// [scanner]
/// poll_interval_ms      = 1000
/// batch_size            = 500
/// polymarket_base_url   = "https://gamma-api.polymarket.com"
/// kalshi_base_url       = "https://api.elections.kalshi.com/trade-api/v2"
/// enabled_sources       = ["polymarket", "kalshi"]
/// request_timeout_ms    = 5000
/// max_retries           = 3
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannerConfig {
    /// Milliseconds between each full poll cycle.
    /// Default: 1 000 (1 s).
    #[serde(default = "ScannerConfig::default_poll_interval_ms")]
    pub poll_interval_ms: u64,

    /// Maximum markets to publish per tick.
    /// Excess results are truncated with a warning.
    /// Default: 500.
    #[serde(default = "ScannerConfig::default_batch_size")]
    pub batch_size: usize,

    /// Base URL for the Polymarket CLOB REST API.
    /// Default: `"https://clob.polymarket.com"`.
    #[serde(default = "ScannerConfig::default_polymarket_base_url")]
    pub polymarket_base_url: String,

    /// Base URL for the Kalshi v2 REST API.
    /// Default: `"https://trading-api.kalshi.com/trade-api/v2"`.
    #[serde(default = "ScannerConfig::default_kalshi_base_url")]
    pub kalshi_base_url: String,

    /// Which data sources to enable.
    /// Valid values: `"polymarket"`, `"kalshi"`.
    /// Default: both enabled.
    #[serde(default = "ScannerConfig::default_enabled_sources")]
    pub enabled_sources: Vec<String>,

    /// Per-request HTTP timeout in milliseconds.
    /// Default: 5 000.
    #[serde(default = "ScannerConfig::default_request_timeout_ms")]
    pub request_timeout_ms: u64,

    /// Maximum consecutive fetch retries before the source is skipped for
    /// this tick.  Default: 3.
    #[serde(default = "ScannerConfig::default_max_retries")]
    pub max_retries: u32,
}

impl ScannerConfig {
    fn default_poll_interval_ms() -> u64 { 1_000 }
    fn default_batch_size() -> usize { 500 }
    fn default_polymarket_base_url() -> String {
        "https://gamma-api.polymarket.com".into()
    }
    fn default_kalshi_base_url() -> String {
        "https://api.elections.kalshi.com/trade-api/v2".into()
    }
    fn default_enabled_sources() -> Vec<String> {
        vec!["polymarket".into(), "kalshi".into()]
    }
    fn default_request_timeout_ms() -> u64 { 5_000 }
    fn default_max_retries() -> u32 { 3 }
}

impl Default for ScannerConfig {
    fn default() -> Self {
        Self {
            poll_interval_ms:   Self::default_poll_interval_ms(),
            batch_size:         Self::default_batch_size(),
            polymarket_base_url: Self::default_polymarket_base_url(),
            kalshi_base_url:    Self::default_kalshi_base_url(),
            enabled_sources:    Self::default_enabled_sources(),
            request_timeout_ms: Self::default_request_timeout_ms(),
            max_retries:        Self::default_max_retries(),
        }
    }
}
