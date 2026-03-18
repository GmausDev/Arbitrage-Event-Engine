// crates/risk_engine/src/error.rs

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RiskEngineError {
    #[error("event bus closed")]
    BusClosed,
}
