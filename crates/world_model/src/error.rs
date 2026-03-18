// crates/world_model/src/error.rs

use thiserror::Error;

// TODO: wire into validation in propagate_update and WorldState::add_dependency.
#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum WorldModelError {
    #[error("market not found: {0}")]
    MarketNotFound(String),

    #[error("invalid probability {0}: must be in [0.0, 1.0]")]
    InvalidProbability(f64),

    #[error("invalid weight {0}: must be in [-1.0, 1.0]")]
    InvalidWeight(f64),

    #[error("invalid confidence {0}: must be in [0.0, 1.0]")]
    InvalidConfidence(f64),

    #[error("constraint not found: {0}")]
    ConstraintNotFound(String),
}
