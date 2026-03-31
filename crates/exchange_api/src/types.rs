// crates/exchange_api/src/types.rs
//
// Shared types for exchange interactions.

use chrono::{DateTime, Utc};
use common::TradeDirection;
use serde::{Deserialize, Serialize};

/// Opaque order identifier returned by an exchange.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OrderId(pub String);

impl std::fmt::Display for OrderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ── Order request ────────────────────────────────────────────────────────────

/// Side of an order on a binary-outcome market.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderSide {
    /// Buy YES tokens (equivalent to `TradeDirection::Buy`).
    BuyYes,
    /// Sell YES tokens / Buy NO tokens (equivalent to `TradeDirection::Sell`).
    BuyNo,
}

impl From<TradeDirection> for OrderSide {
    fn from(d: TradeDirection) -> Self {
        match d {
            TradeDirection::Buy | TradeDirection::Arbitrage => OrderSide::BuyYes,
            TradeDirection::Sell => OrderSide::BuyNo,
        }
    }
}

/// Order type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderType {
    /// Immediately executable at best available price.
    Market,
    /// Resting order at a specific price.
    Limit,
    /// Fill or kill — no partial fills allowed.
    Fok,
    /// Good till cancelled.
    Gtc,
}

/// A request to place an order on an exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderRequest {
    /// The exchange-specific market/ticker identifier.
    pub market_id: String,
    /// Side: buy YES or buy NO.
    pub side: OrderSide,
    /// Order type.
    pub order_type: OrderType,
    /// Size in USD (absolute dollar amount, not a fraction).
    pub size_usd: f64,
    /// Limit price in probability units [0, 1].  Required for `Limit` orders.
    /// For `Market` orders this serves as a worst-acceptable price (slippage cap).
    pub price: Option<f64>,
    /// Originating strategy agent (for attribution).
    pub signal_source: String,
    /// When the original signal was generated.
    pub signal_timestamp: DateTime<Utc>,
}

// ── Order status / response ──────────────────────────────────────────────────

/// Lifecycle state of an order on an exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderStatus {
    /// Submitted but not yet acknowledged by matching engine.
    Pending,
    /// Resting on the order book (limit orders).
    Open,
    /// Partially filled — some quantity executed.
    PartiallyFilled,
    /// Fully filled.
    Filled,
    /// Cancelled by user or system.
    Cancelled,
    /// Rejected by the exchange (insufficient funds, invalid params, etc.).
    Rejected,
    /// Expired (e.g. FOK that could not be filled).
    Expired,
}

/// Snapshot of an order's state as reported by the exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeOrder {
    /// Exchange-assigned order ID.
    pub order_id: OrderId,
    pub market_id: String,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub status: OrderStatus,
    /// Originally requested size in USD.
    pub requested_size_usd: f64,
    /// Cumulative filled size in USD.
    pub filled_size_usd: f64,
    /// Volume-weighted average fill price (probability units).
    pub avg_fill_price: Option<f64>,
    /// Limit price if applicable.
    pub limit_price: Option<f64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ExchangeOrder {
    /// Fraction of the original order that has been filled [0.0, 1.0].
    pub fn fill_ratio(&self) -> f64 {
        if self.requested_size_usd > 0.0 {
            (self.filled_size_usd / self.requested_size_usd).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// True when the order has reached a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            OrderStatus::Filled | OrderStatus::Cancelled | OrderStatus::Rejected | OrderStatus::Expired
        )
    }
}

/// Result of a cancel request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelResult {
    pub order_id: OrderId,
    /// True if the cancellation was accepted.
    pub success: bool,
    /// Amount already filled before cancellation, in USD.
    pub filled_before_cancel_usd: f64,
}

// ── Market data types ────────────────────────────────────────────────────────

/// A single fill/trade on an exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeFill {
    pub market_id: String,
    pub price: f64,
    pub size_usd: f64,
    pub side: OrderSide,
    pub timestamp: DateTime<Utc>,
}

/// Level-2 order book snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeOrderBook {
    pub market_id: String,
    /// Sorted descending by price (best bid first).
    pub bids: Vec<OrderBookLevel>,
    /// Sorted ascending by price (best ask first).
    pub asks: Vec<OrderBookLevel>,
    pub timestamp: DateTime<Utc>,
}

impl ExchangeOrderBook {
    pub fn best_bid(&self) -> Option<f64> {
        self.bids.first().map(|l| l.price)
    }

    pub fn best_ask(&self) -> Option<f64> {
        self.asks.first().map(|l| l.price)
    }

    pub fn mid_price(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(b), Some(a)) => Some((b + a) / 2.0),
            _ => None,
        }
    }

    pub fn spread(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(b), Some(a)) => Some(a - b),
            _ => None,
        }
    }
}

/// A single price level in the order book.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBookLevel {
    /// Price in probability units [0, 1].
    pub price: f64,
    /// Size in USD at this level.
    pub size_usd: f64,
}

// ── Account types ────────────────────────────────────────────────────────────

/// A position held on an exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangePosition {
    pub market_id: String,
    pub side: OrderSide,
    /// Number of contracts/tokens held.
    pub quantity: f64,
    /// Average entry price in probability units.
    pub avg_entry_price: f64,
    /// Current mark-to-market value in USD.
    pub market_value_usd: f64,
}

// ── Credentials ──────────────────────────────────────────────────────────────

/// Exchange credentials loaded from environment variables.
///
/// Each exchange connector defines which fields it requires.
/// Missing fields are detected at connector construction time.
#[derive(Debug, Clone)]
pub struct ExchangeCredentials {
    /// API key / public key.
    pub api_key: Option<String>,
    /// API secret / private key.
    pub api_secret: Option<String>,
    /// Additional auth token (e.g. Kalshi session token, Polymarket API passphrase).
    pub passphrase: Option<String>,
}

impl ExchangeCredentials {
    /// Load credentials from environment variables with the given prefix.
    ///
    /// For prefix `"POLYMARKET"` this reads:
    /// - `POLYMARKET_API_KEY`
    /// - `POLYMARKET_API_SECRET`
    /// - `POLYMARKET_PASSPHRASE`
    pub fn from_env(prefix: &str) -> Self {
        let prefix = prefix.to_uppercase();
        Self {
            api_key:    std::env::var(format!("{prefix}_API_KEY")).ok(),
            api_secret: std::env::var(format!("{prefix}_API_SECRET")).ok(),
            passphrase: std::env::var(format!("{prefix}_PASSPHRASE")).ok(),
        }
    }

    /// Returns an error if the specified field is missing.
    pub fn require_key(&self) -> Result<&str, crate::ExchangeError> {
        self.api_key.as_deref().ok_or(crate::ExchangeError::MissingCredentials(
            "API key not set".into(),
        ))
    }

    pub fn require_secret(&self) -> Result<&str, crate::ExchangeError> {
        self.api_secret.as_deref().ok_or(crate::ExchangeError::MissingCredentials(
            "API secret not set".into(),
        ))
    }
}
