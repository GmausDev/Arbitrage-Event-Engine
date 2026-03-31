// crates/exchange_api/src/lib.rs
//
// Exchange API abstraction layer.
//
// Defines the `ExchangeConnector` trait that all exchange integrations must
// implement, plus shared order/fill types.  The `execution_engine` crate
// consumes this trait to route `ApprovedTrade` events to real exchanges
// (or to the existing `execution_sim` in paper-trading mode).

pub mod config;
pub mod connectors;
pub mod error;
pub mod types;

pub use error::ExchangeError;
pub use types::{
    CancelResult, ExchangeCredentials, ExchangeFill, ExchangeOrder, ExchangeOrderBook,
    OrderId, OrderRequest, OrderSide, OrderStatus, OrderType,
};

use async_trait::async_trait;

/// Unified interface for exchange interactions.
///
/// Each exchange (Polymarket, Kalshi, etc.) implements this trait.
/// Methods return `Result<T, ExchangeError>` for granular error handling.
#[async_trait]
pub trait ExchangeConnector: Send + Sync {
    /// Human-readable exchange name (e.g. "polymarket", "kalshi").
    fn name(&self) -> &str;

    // ── Account ──────────────────────────────────────────────────────────────

    /// Fetch the current account balance in USD.
    async fn get_balance(&self) -> Result<f64, ExchangeError>;

    /// Fetch all open positions on this exchange.
    async fn get_positions(&self) -> Result<Vec<types::ExchangePosition>, ExchangeError>;

    // ── Orders ───────────────────────────────────────────────────────────────

    /// Submit a new order.  Returns the exchange-assigned order ID.
    async fn place_order(&self, req: &OrderRequest) -> Result<OrderId, ExchangeError>;

    /// Cancel an existing order by ID.
    async fn cancel_order(&self, order_id: &OrderId) -> Result<CancelResult, ExchangeError>;

    /// Query the current status of an order.
    async fn get_order_status(&self, order_id: &OrderId) -> Result<ExchangeOrder, ExchangeError>;

    /// List all open (unfilled) orders.
    async fn get_open_orders(&self) -> Result<Vec<ExchangeOrder>, ExchangeError>;

    // ── Market data ──────────────────────────────────────────────────────────

    /// Fetch the current order book for a market.
    async fn get_order_book(&self, market_id: &str) -> Result<ExchangeOrderBook, ExchangeError>;

    /// Fetch recent fills/trades for a market.
    async fn get_recent_fills(&self, market_id: &str, limit: usize) -> Result<Vec<ExchangeFill>, ExchangeError>;

    // ── Exchange info ────────────────────────────────────────────────────────

    /// The fee rate this exchange charges (as a fraction, e.g. 0.02 = 2%).
    /// Used by the cost model to replace the hard-coded default.
    fn fee_rate(&self) -> f64;

    /// Minimum order size in USD for this exchange.
    fn min_order_size_usd(&self) -> f64;
}
