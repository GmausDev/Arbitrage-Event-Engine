// crates/market_graph/src/config.rs
// User-facing graph configuration — a small, TOML-friendly subset of the full
// PropagationConfig.  Load the `[graph]` section of `config/default.toml`
// into this struct, then call `.into()` to obtain a `PropagationConfig`.

use serde::{Deserialize, Serialize};

use crate::types::PropagationConfig;

/// Loadable graph configuration.
///
/// Maps to the `[graph]` table in `config/default.toml`:
///
/// ```toml
/// [graph]
/// change_threshold = 0.01
/// max_depth        = 3
/// ```
///
/// Use [`From<GraphConfig>`] to convert into the full [`PropagationConfig`]
/// expected by [`MarketGraphEngine::with_config`](crate::MarketGraphEngine::with_config).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphConfig {
    /// Minimum `|current_prob − implied_prob|` required to trigger a
    /// propagation pass.  Smaller values propagate more aggressively;
    /// larger values reduce CPU usage on noisy markets.
    ///
    /// Default: `0.01` (1 %).
    #[serde(default = "GraphConfig::default_change_threshold")]
    pub change_threshold: f64,

    /// Maximum BFS hops from the updated market before propagation stops.
    ///
    /// Prevents long chain-reactions in densely connected graphs.
    /// Default: `3`.
    #[serde(default = "GraphConfig::default_max_depth")]
    pub max_depth: usize,
}

impl GraphConfig {
    fn default_change_threshold() -> f64 {
        0.01
    }
    fn default_max_depth() -> usize {
        3
    }
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            change_threshold: Self::default_change_threshold(),
            max_depth: Self::default_max_depth(),
        }
    }
}

/// Convert a [`GraphConfig`] into the full engine [`PropagationConfig`],
/// keeping all other parameters at their defaults.
impl From<GraphConfig> for PropagationConfig {
    fn from(cfg: GraphConfig) -> Self {
        PropagationConfig {
            change_threshold: cfg.change_threshold,
            max_depth: cfg.max_depth,
            ..PropagationConfig::default()
        }
    }
}
