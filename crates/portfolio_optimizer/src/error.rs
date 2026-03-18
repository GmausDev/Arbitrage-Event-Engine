// crates/portfolio_optimizer/src/error.rs

use thiserror::Error;

/// Errors that can be returned by the Portfolio Optimizer.
#[derive(Debug, Error)]
pub enum OptimizerError {
    #[error("event bus closed")]
    BusClosed,
}
