// crates/exchange_api/src/error.rs

use thiserror::Error;

/// Errors returned by exchange connector operations.
#[derive(Debug, Error)]
pub enum ExchangeError {
    /// Exchange-side rejection (insufficient funds, invalid market, etc.).
    #[error("order rejected by exchange: {0}")]
    OrderRejected(String),

    /// Required credentials are not configured.
    #[error("missing credentials: {0}")]
    MissingCredentials(String),

    /// Authentication failed (invalid key, expired token, etc.).
    #[error("authentication failed: {0}")]
    AuthFailed(String),

    /// Rate limited by the exchange.
    #[error("rate limited: retry after {retry_after_ms}ms")]
    RateLimited { retry_after_ms: u64 },

    /// HTTP or network error.
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    /// Unexpected response format.
    #[error("response parse error: {0}")]
    ParseError(String),

    /// The requested resource was not found (unknown market, unknown order).
    #[error("not found: {0}")]
    NotFound(String),

    /// Exchange is in maintenance or unavailable.
    #[error("exchange unavailable: {0}")]
    Unavailable(String),

    /// Insufficient funds to place the order.
    #[error("insufficient funds: available=${available:.2}, required=${required:.2}")]
    InsufficientFunds { available: f64, required: f64 },

    /// Catch-all for unexpected errors.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
