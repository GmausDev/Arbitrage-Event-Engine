// crates/exchange_api/src/connectors/kalshi.rs
//
// Kalshi REST API connector.
//
// Kalshi uses a standard REST API with email/password or API-key authentication:
//   1. POST /login  → returns a session token
//   2. All subsequent requests carry the token in the Authorization header
//
// This connector targets the trading API (trading-api.kalshi.com) — NOT the
// public elections API used by the data ingestion layer.
//
// Environment variables required:
//   KALSHI_API_KEY     — API key (or email)
//   KALSHI_API_SECRET  — API secret (or password)
//
// API documentation: https://trading-api.readme.kalshi.com/

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::RwLock;
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::{
    error::ExchangeError,
    types::*,
    ExchangeConnector,
};

// ── API request/response types ───────────────────────────────────────────────

#[derive(Serialize)]
struct LoginRequest {
    email: String,
    password: String,
}

#[derive(Deserialize)]
struct LoginResponse {
    token: String,
    #[serde(default)]
    member_id: String,
}

#[derive(Serialize)]
struct KalshiOrderPayload {
    ticker: String,
    /// "yes" or "no"
    side: String,
    /// "market" or "limit"
    #[serde(rename = "type")]
    order_type: String,
    /// Number of contracts.
    count: i64,
    /// Limit price in cents (1-99).  Omit for market orders.
    #[serde(skip_serializing_if = "Option::is_none")]
    yes_price: Option<i64>,
    /// "ioc" (immediate-or-cancel) | "gtc" (good-till-cancelled)
    #[serde(skip_serializing_if = "Option::is_none")]
    action: Option<String>,
}

#[derive(Deserialize)]
struct KalshiOrderResponse {
    order: KalshiOrderData,
}

#[derive(Deserialize)]
struct KalshiOrderData {
    order_id: String,
    #[serde(default)]
    ticker: String,
    #[serde(default)]
    side: String,
    #[serde(rename = "type", default)]
    order_type: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    yes_price: i64,
    #[serde(default)]
    remaining_count: i64,
    #[serde(default)]
    place_count: i64,
    #[serde(default)]
    created_time: Option<String>,
}

#[derive(Deserialize)]
struct KalshiOrdersResponse {
    #[serde(default)]
    orders: Vec<KalshiOrderData>,
}

#[derive(Deserialize)]
struct KalshiBalanceResponse {
    balance: i64, // in cents
}

#[derive(Deserialize)]
struct KalshiPositionsResponse {
    #[serde(default)]
    market_positions: Vec<KalshiPosition>,
}

#[derive(Deserialize)]
struct KalshiPosition {
    #[serde(default)]
    ticker: String,
    #[serde(default)]
    market_exposure: i64, // in cents
    #[serde(default)]
    total_traded: i64,
    #[serde(default)]
    resting_orders_count: i64,
    #[serde(default)]
    position: i64, // positive = yes, negative = no
    #[serde(default)]
    realized_pnl: i64,
}

#[derive(Deserialize)]
struct KalshiOrderBookResponse {
    orderbook: KalshiOrderBookData,
}

#[derive(Deserialize)]
struct KalshiOrderBookData {
    #[serde(default)]
    yes: Vec<Vec<serde_json::Value>>,
    #[serde(default)]
    no: Vec<Vec<serde_json::Value>>,
}

#[derive(Deserialize)]
struct KalshiFillsResponse {
    #[serde(default)]
    fills: Vec<KalshiFill>,
}

#[derive(Deserialize)]
struct KalshiFill {
    #[serde(default)]
    ticker: String,
    #[serde(default)]
    yes_price: i64,
    #[serde(default)]
    count: i64,
    #[serde(default)]
    side: String,
    #[serde(default)]
    created_time: Option<String>,
}

// ── Connector ────────────────────────────────────────────────────────────────

/// Kalshi REST API exchange connector.
pub struct KalshiConnector {
    client: reqwest::Client,
    base_url: String,
    credentials: ExchangeCredentials,
    /// Cached session token (refreshed on 401).
    session_token: RwLock<Option<String>>,
    member_id: RwLock<Option<String>>,
}

impl KalshiConnector {
    const DEFAULT_BASE_URL: &'static str = "https://trading-api.kalshi.com/trade-api/v2";
    /// Kalshi charges a fee on profitable settlements only — modeled as ~2%
    /// of expected win (7% settlement fee on binary markets with typical edge).
    const FEE_RATE: f64 = 0.02;
    /// Minimum order is 1 contract ($1).
    const MIN_ORDER_USD: f64 = 1.0;
    /// Each Kalshi contract pays $1 on resolution.
    const DOLLARS_PER_CONTRACT: f64 = 1.0;

    pub fn new(credentials: ExchangeCredentials) -> Result<Self, ExchangeError> {
        credentials.require_key()?;
        credentials.require_secret()?;

        let client = reqwest::ClientBuilder::new()
            .timeout(Duration::from_secs(10))
            .user_agent("prediction-market-bot/1.0")
            .build()
            .map_err(|e| ExchangeError::Other(e.into()))?;

        Ok(Self {
            client,
            base_url: Self::DEFAULT_BASE_URL.to_string(),
            credentials,
            session_token: RwLock::new(None),
            member_id: RwLock::new(None),
        })
    }

    /// Authenticate and cache the session token.
    async fn login(&self) -> Result<String, ExchangeError> {
        let email = self.credentials.require_key()?;
        let password = self.credentials.require_secret()?;

        let url = format!("{}/login", self.base_url);
        let resp = self.client
            .post(&url)
            .json(&LoginRequest {
                email: email.to_string(),
                password: password.to_string(),
            })
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(ExchangeError::AuthFailed(
                format!("Kalshi login failed: HTTP {}", resp.status()),
            ));
        }

        let login: LoginResponse = resp.json().await.map_err(|e| {
            ExchangeError::ParseError(format!("login response parse error: {e}"))
        })?;

        info!("kalshi: authenticated as member {}", login.member_id);

        // Cache token.
        if let Ok(mut t) = self.session_token.write() {
            *t = Some(login.token.clone());
        }
        if let Ok(mut m) = self.member_id.write() {
            *m = Some(login.member_id);
        }

        Ok(login.token)
    }

    /// Get the current session token, logging in if needed.
    async fn get_token(&self) -> Result<String, ExchangeError> {
        {
            if let Ok(guard) = self.session_token.read() {
                if let Some(ref token) = *guard {
                    return Ok(token.clone());
                }
            }
        }
        self.login().await
    }

    /// Build an authenticated GET request.
    async fn auth_get(&self, url: &str) -> Result<reqwest::Response, ExchangeError> {
        let token = self.get_token().await?;
        let resp = self.client
            .get(url)
            .bearer_auth(&token)
            .send()
            .await?;

        // On 401, try re-authenticating once.
        if resp.status() == 401 {
            warn!("kalshi: session expired, re-authenticating");
            let new_token = self.login().await?;
            return Ok(self.client.get(url).bearer_auth(&new_token).send().await?);
        }

        Ok(resp)
    }

    fn parse_order_status(status: &str) -> OrderStatus {
        match status.to_lowercase().as_str() {
            "resting" | "open"   => OrderStatus::Open,
            "pending"            => OrderStatus::Pending,
            "executed" | "filled"=> OrderStatus::Filled,
            "canceled"           => OrderStatus::Cancelled,
            "expired"            => OrderStatus::Expired,
            _                    => OrderStatus::Pending,
        }
    }

    fn parse_side(side: &str) -> OrderSide {
        if side.eq_ignore_ascii_case("no") {
            OrderSide::BuyNo
        } else {
            OrderSide::BuyYes
        }
    }

    /// Convert a dollar amount to number of contracts at a given price.
    fn usd_to_contracts(size_usd: f64, price_cents: i64) -> i64 {
        if price_cents <= 0 { return 0; }
        let price_dollars = price_cents as f64 / 100.0;
        (size_usd / price_dollars).floor() as i64
    }
}

#[async_trait]
impl ExchangeConnector for KalshiConnector {
    fn name(&self) -> &str {
        "kalshi"
    }

    async fn get_balance(&self) -> Result<f64, ExchangeError> {
        let url = format!("{}/portfolio/balance", self.base_url);
        let resp = self.auth_get(&url).await?;

        let body: KalshiBalanceResponse = resp.json().await.map_err(|e| {
            ExchangeError::ParseError(format!("balance parse error: {e}"))
        })?;

        Ok(body.balance as f64 / 100.0) // cents → dollars
    }

    async fn get_positions(&self) -> Result<Vec<ExchangePosition>, ExchangeError> {
        let member_id = {
            self.member_id.read().ok().and_then(|g| g.clone())
        };
        let member_id = match member_id {
            Some(id) => id,
            None => {
                self.login().await?;
                self.member_id.read().ok().and_then(|g| g.clone())
                    .ok_or_else(|| ExchangeError::AuthFailed("no member_id after login".into()))?
            }
        };

        let url = format!("{}/portfolio/positions", self.base_url);
        let resp = self.auth_get(&url).await?;

        let body: KalshiPositionsResponse = resp.json().await.map_err(|e| {
            ExchangeError::ParseError(format!("positions parse error: {e}"))
        })?;

        Ok(body.market_positions
            .into_iter()
            .filter(|p| p.position != 0)
            .map(|p| {
                let side = if p.position > 0 { OrderSide::BuyYes } else { OrderSide::BuyNo };
                ExchangePosition {
                    market_id:        p.ticker,
                    side,
                    quantity:         p.position.unsigned_abs() as f64,
                    avg_entry_price:  0.0, // Kalshi doesn't return avg entry in positions
                    market_value_usd: p.market_exposure as f64 / 100.0,
                }
            })
            .collect())
    }

    async fn place_order(&self, req: &OrderRequest) -> Result<OrderId, ExchangeError> {
        let token = self.get_token().await?;

        let side_str = match req.side {
            OrderSide::BuyYes => "yes",
            OrderSide::BuyNo  => "no",
        };

        let (ot, price_cents) = match req.order_type {
            OrderType::Market | OrderType::Fok => {
                // Market orders on Kalshi: use IOC with aggressive price.
                let p = match req.side {
                    OrderSide::BuyYes => 99, // worst price for yes
                    OrderSide::BuyNo  => 1,  // worst price for no
                };
                ("market", p)
            }
            OrderType::Limit | OrderType::Gtc => {
                let p = req.price.map(|v| (v * 100.0).round() as i64).unwrap_or(50);
                ("limit", p.clamp(1, 99))
            }
        };

        let count = Self::usd_to_contracts(req.size_usd, price_cents);
        if count <= 0 {
            return Err(ExchangeError::OrderRejected(
                format!("computed 0 contracts for ${:.2} at {}¢", req.size_usd, price_cents),
            ));
        }

        let payload = KalshiOrderPayload {
            ticker:     req.market_id.clone(),
            side:       side_str.to_string(),
            order_type: ot.to_string(),
            count,
            yes_price:  Some(price_cents),
            action:     None,
        };

        let url = format!("{}/portfolio/orders", self.base_url);
        let resp = self.client
            .post(&url)
            .bearer_auth(&token)
            .json(&payload)
            .send()
            .await?;

        if resp.status() == 429 {
            return Err(ExchangeError::RateLimited { retry_after_ms: 1000 });
        }
        if resp.status() == 401 || resp.status() == 403 {
            return Err(ExchangeError::AuthFailed("Kalshi API authentication failed".into()));
        }

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ExchangeError::OrderRejected(
                format!("HTTP {status}: {body}"),
            ));
        }

        let body: KalshiOrderResponse = resp.json().await.map_err(|e| {
            ExchangeError::ParseError(format!("order response parse error: {e}"))
        })?;

        debug!(order_id = %body.order.order_id, "kalshi: order placed");
        metrics::counter!("exchange_orders_placed_total", "exchange" => "kalshi").increment(1);
        Ok(OrderId(body.order.order_id))
    }

    async fn cancel_order(&self, order_id: &OrderId) -> Result<CancelResult, ExchangeError> {
        let token = self.get_token().await?;
        let url = format!("{}/portfolio/orders/{}", self.base_url, order_id.0);

        let resp = self.client
            .delete(&url)
            .bearer_auth(&token)
            .send()
            .await?;

        let success = resp.status().is_success();
        Ok(CancelResult {
            order_id: order_id.clone(),
            success,
            filled_before_cancel_usd: 0.0,
        })
    }

    async fn get_order_status(&self, order_id: &OrderId) -> Result<ExchangeOrder, ExchangeError> {
        let url = format!("{}/portfolio/orders/{}", self.base_url, order_id.0);
        let resp = self.auth_get(&url).await?;

        if resp.status() == 404 {
            return Err(ExchangeError::NotFound(format!("order {} not found", order_id)));
        }

        let body: KalshiOrderResponse = resp.json().await.map_err(|e| {
            ExchangeError::ParseError(format!("order status parse error: {e}"))
        })?;

        let o = body.order;
        let filled_count = o.place_count - o.remaining_count;
        let price_usd = o.yes_price as f64 / 100.0;

        Ok(ExchangeOrder {
            order_id:           OrderId(o.order_id),
            market_id:          o.ticker,
            side:               Self::parse_side(&o.side),
            order_type:         if o.order_type == "market" { OrderType::Market } else { OrderType::Limit },
            status:             Self::parse_order_status(&o.status),
            requested_size_usd: o.place_count as f64 * price_usd,
            filled_size_usd:    filled_count as f64 * price_usd,
            avg_fill_price:     if filled_count > 0 { Some(price_usd) } else { None },
            limit_price:        Some(price_usd),
            created_at:         Utc::now(),
            updated_at:         Utc::now(),
        })
    }

    async fn get_open_orders(&self) -> Result<Vec<ExchangeOrder>, ExchangeError> {
        let url = format!("{}/portfolio/orders?status=resting", self.base_url);
        let resp = self.auth_get(&url).await?;

        let body: KalshiOrdersResponse = resp.json().await.map_err(|e| {
            ExchangeError::ParseError(format!("open orders parse error: {e}"))
        })?;

        Ok(body.orders.into_iter().map(|o| {
            let filled_count = o.place_count - o.remaining_count;
            let price_usd = o.yes_price as f64 / 100.0;
            ExchangeOrder {
                order_id:           OrderId(o.order_id),
                market_id:          o.ticker,
                side:               Self::parse_side(&o.side),
                order_type:         if o.order_type == "market" { OrderType::Market } else { OrderType::Limit },
                status:             Self::parse_order_status(&o.status),
                requested_size_usd: o.place_count as f64 * price_usd,
                filled_size_usd:    filled_count as f64 * price_usd,
                avg_fill_price:     if filled_count > 0 { Some(price_usd) } else { None },
                limit_price:        Some(price_usd),
                created_at:         Utc::now(),
                updated_at:         Utc::now(),
            }
        }).collect())
    }

    async fn get_order_book(&self, market_id: &str) -> Result<ExchangeOrderBook, ExchangeError> {
        let url = format!("{}/orderbook/{}", self.base_url, market_id);
        let resp = self.auth_get(&url).await?;

        let body: KalshiOrderBookResponse = resp.json().await.map_err(|e| {
            ExchangeError::ParseError(format!("order book parse error: {e}"))
        })?;

        let parse_levels = |levels: Vec<Vec<serde_json::Value>>| -> Vec<OrderBookLevel> {
            levels.into_iter().filter_map(|pair| {
                let price = pair.first()?.as_f64()? / 100.0; // cents → probability
                let size = pair.get(1)?.as_f64()?;
                Some(OrderBookLevel { price, size_usd: size })
            }).collect()
        };

        // Kalshi: `yes` bids are our bids, `no` bids map to asks on the YES side.
        let bids = parse_levels(body.orderbook.yes);
        let asks = parse_levels(body.orderbook.no);

        Ok(ExchangeOrderBook {
            market_id: market_id.to_string(),
            bids,
            asks,
            timestamp: Utc::now(),
        })
    }

    async fn get_recent_fills(&self, market_id: &str, limit: usize) -> Result<Vec<ExchangeFill>, ExchangeError> {
        let url = format!("{}/portfolio/fills?ticker={}&limit={}", self.base_url, market_id, limit);
        let resp = self.auth_get(&url).await?;

        let body: KalshiFillsResponse = resp.json().await.map_err(|e| {
            ExchangeError::ParseError(format!("fills parse error: {e}"))
        })?;

        Ok(body.fills.into_iter().map(|f| {
            ExchangeFill {
                market_id: f.ticker,
                price:     f.yes_price as f64 / 100.0,
                size_usd:  f.count as f64 * (f.yes_price as f64 / 100.0),
                side:      Self::parse_side(&f.side),
                timestamp: Utc::now(),
            }
        }).collect())
    }

    fn fee_rate(&self) -> f64 {
        Self::FEE_RATE
    }

    fn min_order_size_usd(&self) -> f64 {
        Self::MIN_ORDER_USD
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usd_to_contracts_basic() {
        assert_eq!(KalshiConnector::usd_to_contracts(10.0, 50), 20);
        assert_eq!(KalshiConnector::usd_to_contracts(5.0, 25), 20);
        assert_eq!(KalshiConnector::usd_to_contracts(1.0, 99), 1);
        assert_eq!(KalshiConnector::usd_to_contracts(0.5, 99), 0);
    }

    #[test]
    fn usd_to_contracts_edge_cases() {
        assert_eq!(KalshiConnector::usd_to_contracts(10.0, 0), 0);
        assert_eq!(KalshiConnector::usd_to_contracts(10.0, -5), 0);
    }

    #[test]
    fn parse_side_mapping() {
        assert_eq!(KalshiConnector::parse_side("yes"), OrderSide::BuyYes);
        assert_eq!(KalshiConnector::parse_side("no"), OrderSide::BuyNo);
    }

    #[test]
    fn parse_status_mapping() {
        assert_eq!(KalshiConnector::parse_order_status("resting"), OrderStatus::Open);
        assert_eq!(KalshiConnector::parse_order_status("executed"), OrderStatus::Filled);
        assert_eq!(KalshiConnector::parse_order_status("canceled"), OrderStatus::Cancelled);
    }
}
