// prediction-agent/src/app_config.rs
// Loads settings from config/default.toml (and optional config/local.toml).

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Top-level application configuration.
/// Mirrors the keys in config/default.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Markets to track (niche tags, e.g. ["crypto", "politics"])
    pub target_niches: Vec<String>,

    /// Minimum model vs. market probability gap to trigger an arb signal.
    /// Mirrors Configuration & Monitoring note: default 0.05
    pub arb_threshold: f64,

    /// Event Bus channel capacity
    pub bus_capacity: usize,

    /// Market scanner poll interval in milliseconds
    pub tick_interval_ms: u64,

    /// Paper-trading starting capital in USD. Used by portfolio_engine, execution_sim,
    /// portfolio_optimizer, risk_engine, and performance_analytics. No real money is ever sent.
    pub paper_bankroll: f64,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            target_niches: vec![
                "crypto".into(),
                "politics".into(),
                "geopolitics".into(),
                "macro".into(),
                "sports".into(),
            ],
            arb_threshold: 0.05,
            bus_capacity: 1_024,
            tick_interval_ms: 1_000,
            paper_bankroll: 10_000.0,
        }
    }
}

impl AppConfig {
    /// Load configuration from config/default.toml.
    /// Falls back to `AppConfig::default()` if the file is missing.
    pub fn load() -> Result<Self> {
        let cfg = config::Config::builder()
            .add_source(config::File::with_name("config/default").required(false))
            .add_source(config::File::with_name("config/local").required(false))
            .add_source(config::Environment::with_prefix("PRED_AGENT"))
            .build()?;

        Ok(cfg.try_deserialize()?)
    }
}
