// crates/bayesian_engine/src/error.rs
// Error types for the Bayesian Probability Engine.

use thiserror::Error;

/// Errors that can occur during Bayesian engine operations.
#[derive(Debug, Error, PartialEq)]
pub enum BayesianError {
    /// A market ID was referenced that has no [`BeliefState`] in the engine.
    ///
    /// Call [`BayesianEngine::ensure_belief`] or wait for a `MarketUpdate`
    /// event to seed the engine with this market before operating on it.
    #[error("market not found: '{0}'")]
    MarketNotFound(String),

    /// A probability value outside `[0.0, 1.0]` was supplied.
    #[error("invalid probability {0}: must be in [0.0, 1.0]")]
    InvalidProbability(f64),

    /// A precision/confidence value outside `[0.0, 1.0]` was supplied.
    #[error("invalid precision {0}: must be in [0.0, 1.0]")]
    InvalidPrecision(f64),

    /// [`get_signal_and_kelly`](crate::BayesianEngine::get_signal_and_kelly) was
    /// called for a market that has never had a market price fused via
    /// [`fuse_market_price`](crate::BayesianEngine::fuse_market_price).
    /// A reference price is required to compute payout odds and the log-odds edge.
    #[error("no market price available for '{0}': call fuse_market_price first")]
    MissingMarketPrice(String),
}
