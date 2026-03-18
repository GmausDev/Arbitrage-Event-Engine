// crates/market_graph/src/error.rs

use thiserror::Error;

#[derive(Debug, Error)]
pub enum GraphError {
    #[error("market not found: `{0}`")]
    MarketNotFound(String),

    #[error("edge not found: `{0}` → `{1}`")]
    EdgeNotFound(String, String),

    #[error("invalid probability {0:.4}: must be in [0.0, 1.0]")]
    InvalidProbability(f64),

    #[error("invalid weight {0:.4}: must be in [-1.0, 1.0]")]
    InvalidWeight(f64),

    #[error("invalid confidence {0:.4}: must be in [0.0, 1.0]")]
    InvalidConfidence(f64),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}
