// control-panel/src/config_store.rs
//
// Live configuration store.  Updated via POST /api/control/update_parameter
// without restarting the system.  All engines read from here instead of
// static config files when they need a hot-reloadable value.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// TradingConfig
// ---------------------------------------------------------------------------

/// Full set of hot-reloadable trading parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingConfig {
    /// Minimum expected value (edge) required before a signal is acted on.
    pub edge_threshold: f64,

    /// Maximum fraction of bankroll allocated to a single position.
    pub max_position_size: f64,

    /// Fraction of Kelly to apply (full Kelly = 1.0; quarter Kelly = 0.25).
    pub kelly_fraction: f64,

    /// Minimum model confidence [0, 1] to emit a signal.
    pub signal_confidence: f64,

    /// Maximum total portfolio exposure fraction.
    pub risk_threshold: f64,

    /// Per-agent enable flags.  Key = agent name, value = enabled.
    pub agent_enable_flags: HashMap<String, bool>,
}

impl Default for TradingConfig {
    fn default() -> Self {
        let mut flags = HashMap::new();
        for agent in &[
            "bayesian_engine",
            "temporal_agent",
            "relationship_discovery",
            "graph_arb_agent",
            "bayesian_edge_agent",
            "signal_agent",
            "meta_strategy",
        ] {
            flags.insert(agent.to_string(), true);
        }
        Self {
            edge_threshold:     0.02,
            max_position_size:  0.05,
            kelly_fraction:     0.25,
            signal_confidence:  0.30,
            risk_threshold:     0.20,
            agent_enable_flags: flags,
        }
    }
}

// ---------------------------------------------------------------------------
// Patch struct — only the fields the caller wants to change
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingConfigPatch {
    pub edge_threshold:     Option<f64>,
    pub max_position_size:  Option<f64>,
    pub kelly_fraction:     Option<f64>,
    pub signal_confidence:  Option<f64>,
    pub risk_threshold:     Option<f64>,
    pub agent_name:         Option<String>,
    pub agent_enabled:      Option<bool>,
}

// ---------------------------------------------------------------------------
// ConfigStore
// ---------------------------------------------------------------------------

/// Thread-safe, hot-reloadable configuration store.
pub struct ConfigStore {
    inner: RwLock<TradingConfig>,
}

impl ConfigStore {
    pub fn new(initial: TradingConfig) -> Self {
        Self { inner: RwLock::new(initial) }
    }

    /// Snapshot the current configuration.
    pub async fn get(&self) -> TradingConfig {
        self.inner.read().await.clone()
    }

    /// Apply a partial patch.
    pub async fn patch(&self, p: TradingConfigPatch) {
        let mut cfg = self.inner.write().await;
        if let Some(v) = p.edge_threshold    { cfg.edge_threshold    = v; }
        if let Some(v) = p.max_position_size { cfg.max_position_size = v; }
        if let Some(v) = p.kelly_fraction    { cfg.kelly_fraction    = v; }
        if let Some(v) = p.signal_confidence { cfg.signal_confidence = v; }
        if let Some(v) = p.risk_threshold    { cfg.risk_threshold    = v; }
        if let (Some(name), Some(enabled)) = (p.agent_name, p.agent_enabled) {
            cfg.agent_enable_flags.insert(name, enabled);
        }
    }
}

impl Default for ConfigStore {
    fn default() -> Self {
        Self::new(TradingConfig::default())
    }
}
