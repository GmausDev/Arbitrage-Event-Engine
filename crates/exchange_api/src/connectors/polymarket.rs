// crates/exchange_api/src/connectors/polymarket.rs
//
// Polymarket CLOB (Central Limit Order Book) connector.
//
// Polymarket uses a hybrid model:
//   - Market data: Gamma REST API (public, no auth)
//   - Order placement: CLOB API via signed EIP-712 messages
//
// This connector implements the ExchangeConnector trait for live trading.
//
// Environment variables required:
//   POLYMARKET_API_KEY       — CLOB API key
//   POLYMARKET_API_SECRET    — CLOB API secret
//   POLYMARKET_PASSPHRASE    — CLOB API passphrase
//
// API documentation: https://docs.polymarket.com/

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, warn};

use crate::{
    error::ExchangeError,
    types::*,
    ExchangeConnector,
};

// ── API response types ───────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ClobOrderResponse {
    #[serde(rename = "orderID", default)]
    order_id: String,
    #[serde(default)]
    success: bool,
    #[serde(rename = "errorMsg", default)]
    error_msg: String,
}

#[derive(Deserialize)]
struct ClobOpenOrder {
    id: String,
    #[serde(default)]
    asset_id: String,
    #[serde(default)]
    side: String,
    #[serde(default)]
    price: String,
    #[serde(rename = "original_size", default)]
    original_size: String,
    #[serde(rename = "size_matched", default)]
    size_matched: String,
    #[serde(default)]
    status: String,
    #[serde(rename = "created_at", default)]
    created_at: String,
}

#[derive(Deserialize)]
struct ClobCancelResponse {
    #[serde(default)]
    canceled: Vec<String>,
    #[serde(default)]
    not_canceled: Vec<String>,
}

#[derive(Deserialize)]
struct ClobTradeResponse {
    #[serde(default)]
    id: String,
    #[serde(default)]
    price: String,
    #[serde(default)]
    size: String,
    #[serde(default)]
    side: String,
    #[serde(rename = "created_at", default)]
    created_at: String,
}

#[derive(Deserialize)]
struct ClobOrderBookResponse {
    #[serde(default)]
    bids: Vec<ClobBookLevel>,
    #[serde(default)]
    asks: Vec<ClobBookLevel>,
}

#[derive(Deserialize)]
struct ClobBookLevel {
    price: String,
    size: String,
}

#[derive(Serialize)]
struct ClobOrderPayload {
    /// Token ID (condition ID for the YES/NO outcome).
    #[serde(rename = "tokenID")]
    token_id: String,
    price: String,
    size: String,
    side: String,
    /// "GTC" | "FOK" | "GTD"
    #[serde(rename = "orderType")]
    order_type: String,
}

// ── Connector ────────────────────────────────────────────────────────────────

/// Polymarket CLOB exchange connector.
pub struct PolymarketConnector {
    client: reqwest::Client,
    /// CLOB API base URL.
    clob_base: String,
    /// Gamma API base URL (public market data).
    gamma_base: String,
    credentials: ExchangeCredentials,
}

impl PolymarketConnector {
    /// CLOB API base URL.
    const DEFAULT_CLOB_BASE: &'static str = "https://clob.polymarket.com";
    /// Gamma API base URL for market data.
    const DEFAULT_GAMMA_BASE: &'static str = "https://gamma-api.polymarket.com";
    /// Polymarket charges no explicit fee on CLOB; takers pay ~1-2% spread.
    /// We model this as 0.5% (conservative estimate of typical execution cost).
    const FEE_RATE: f64 = 0.005;
    /// Minimum order size on Polymarket.
    const MIN_ORDER_USD: f64 = 1.0;

    pub fn new(credentials: ExchangeCredentials) -> Result<Self, ExchangeError> {
        // Validate that we have the required credentials.
        credentials.require_key()?;
        credentials.require_secret()?;

        let client = reqwest::ClientBuilder::new()
            .timeout(Duration::from_secs(10))
            .user_agent("prediction-market-bot/1.0")
            .build()
            .map_err(|e| ExchangeError::Other(e.into()))?;

        Ok(Self {
            client,
            clob_base: Self::DEFAULT_CLOB_BASE.to_string(),
            gamma_base: Self::DEFAULT_GAMMA_BASE.to_string(),
            credentials,
        })
    }

    /// Build authenticated headers for CLOB API requests.
    fn auth_headers(&self) -> reqwest::header::HeaderMap {
        use reqwest::header::HeaderValue;

        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(ref key) = self.credentials.api_key {
            if let Ok(v) = HeaderValue::from_str(key) {
                headers.insert("POLY-ADDRESS", v);
            }
        }
        if let Some(ref secret) = self.credentials.api_secret {
            if let Ok(v) = HeaderValue::from_str(secret) {
                headers.insert("POLY-SIGNATURE", v);
            }
        }
        if let Some(ref pass) = self.credentials.passphrase {
            if let Ok(v) = HeaderValue::from_str(pass) {
                headers.insert("POLY-PASSPHRASE", v);
            }
        }
        let ts = Utc::now().timestamp_millis().to_string();
        if let Ok(v) = HeaderValue::from_str(&ts) {
            headers.insert("POLY-TIMESTAMP", v);
        }
        headers
    }

    fn parse_order_status(status: &str) -> OrderStatus {
        match status.to_lowercase().as_str() {
            "live" | "open" => OrderStatus::Open,
            "matched" | "filled" => OrderStatus::Filled,
            "cancelled" | "canceled" => OrderStatus::Cancelled,
            "expired" => OrderStatus::Expired,
            _ => OrderStatus::Pending,
        }
    }

    fn parse_side(side: &str) -> OrderSide {
        if side.eq_ignore_ascii_case("sell") || side.eq_ignore_ascii_case("no") {
            OrderSide::BuyNo
        } else {
            OrderSide::BuyYes
        }
    }
}

#[async_trait]
impl ExchangeConnector for PolymarketConnector {
    fn name(&self) -> &str {
        "polymarket"
    }

    async fn get_balance(&self) -> Result<f64, ExchangeError> {
        // Polymarket balance is on-chain (USDC).  The CLOB API doesn't expose
        // a direct balance endpoint — in production this would call a wallet
        // RPC or the funding API.  For now, return a placeholder that the OMS
        // will override with cached state.
        warn!("polymarket: get_balance requires on-chain USDC query — returning 0.0 placeholder");
        Ok(0.0)
    }

    async fn get_positions(&self) -> Result<Vec<ExchangePosition>, ExchangeError> {
        // Positions on Polymarket are ERC-1155 token balances.
        // The CLOB API has no direct positions endpoint — this requires
        // querying the conditional token framework contract.
        warn!("polymarket: get_positions requires on-chain token query — returning empty");
        Ok(vec![])
    }

    async fn place_order(&self, req: &OrderRequest) -> Result<OrderId, ExchangeError> {
        let side_str = match req.side {
            OrderSide::BuyYes => "BUY",
            OrderSide::BuyNo  => "SELL",
        };
        let ot = match req.order_type {
            OrderType::Fok    => "FOK",
            OrderType::Market => "FOK", // market orders use FOK on CLOB
            _                 => "GTC",
        };
        let price = req.price.unwrap_or(0.99);

        let payload = ClobOrderPayload {
            token_id:   req.market_id.clone(),
            price:      format!("{:.4}", price),
            size:       format!("{:.2}", req.size_usd),
            side:       side_str.to_string(),
            order_type: ot.to_string(),
        };

        let url = format!("{}/order", self.clob_base);
        let resp = self.client
            .post(&url)
            .headers(self.auth_headers())
            .json(&payload)
            .send()
            .await?;

        if resp.status() == 429 {
            return Err(ExchangeError::RateLimited { retry_after_ms: 1000 });
        }
        if resp.status() == 401 || resp.status() == 403 {
            return Err(ExchangeError::AuthFailed("CLOB API authentication failed".into()));
        }

        let status = resp.status();
        let body: ClobOrderResponse = resp.json().await.map_err(|e| {
            ExchangeError::ParseError(format!("failed to parse CLOB order response: {e}"))
        })?;

        if !body.success || !status.is_success() {
            return Err(ExchangeError::OrderRejected(
                if body.error_msg.is_empty() {
                    format!("HTTP {status}")
                } else {
                    body.error_msg
                },
            ));
        }

        debug!(order_id = %body.order_id, "polymarket: order placed");
        metrics::counter!("exchange_orders_placed_total", "exchange" => "polymarket").increment(1);
        Ok(OrderId(body.order_id))
    }

    async fn cancel_order(&self, order_id: &OrderId) -> Result<CancelResult, ExchangeError> {
        let url = format!("{}/order/{}", self.clob_base, order_id.0);
        let resp = self.client
            .delete(&url)
            .headers(self.auth_headers())
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(ExchangeError::OrderRejected(
                format!("cancel failed: HTTP {}", resp.status()),
            ));
        }

        let body: ClobCancelResponse = resp.json().await.map_err(|e| {
            ExchangeError::ParseError(format!("cancel response parse error: {e}"))
        })?;

        let success = body.canceled.contains(&order_id.0);
        Ok(CancelResult {
            order_id: order_id.clone(),
            success,
            filled_before_cancel_usd: 0.0, // CLOB doesn't report this on cancel
        })
    }

    async fn get_order_status(&self, order_id: &OrderId) -> Result<ExchangeOrder, ExchangeError> {
        let url = format!("{}/order/{}", self.clob_base, order_id.0);
        let resp = self.client
            .get(&url)
            .headers(self.auth_headers())
            .send()
            .await?;

        if resp.status() == 404 {
            return Err(ExchangeError::NotFound(format!("order {} not found", order_id)));
        }

        let raw: ClobOpenOrder = resp.json().await.map_err(|e| {
            ExchangeError::ParseError(format!("order status parse error: {e}"))
        })?;

        let original: f64 = raw.original_size.parse().unwrap_or(0.0);
        let matched: f64  = raw.size_matched.parse().unwrap_or(0.0);
        let price: f64    = raw.price.parse().unwrap_or(0.0);

        Ok(ExchangeOrder {
            order_id:           OrderId(raw.id),
            market_id:          raw.asset_id,
            side:               Self::parse_side(&raw.side),
            order_type:         OrderType::Limit,
            status:             Self::parse_order_status(&raw.status),
            requested_size_usd: original,
            filled_size_usd:    matched,
            avg_fill_price:     if matched > 0.0 { Some(price) } else { None },
            limit_price:        Some(price),
            created_at:         Utc::now(), // TODO: parse raw.created_at
            updated_at:         Utc::now(),
        })
    }

    async fn get_open_orders(&self) -> Result<Vec<ExchangeOrder>, ExchangeError> {
        let url = format!("{}/orders", self.clob_base);
        let resp = self.client
            .get(&url)
            .headers(self.auth_headers())
            .send()
            .await?;

        let orders: Vec<ClobOpenOrder> = resp.json().await.map_err(|e| {
            ExchangeError::ParseError(format!("open orders parse error: {e}"))
        })?;

        Ok(orders
            .into_iter()
            .map(|raw| {
                let original: f64 = raw.original_size.parse().unwrap_or(0.0);
                let matched: f64  = raw.size_matched.parse().unwrap_or(0.0);
                let price: f64    = raw.price.parse().unwrap_or(0.0);
                ExchangeOrder {
                    order_id:           OrderId(raw.id),
                    market_id:          raw.asset_id,
                    side:               Self::parse_side(&raw.side),
                    order_type:         OrderType::Limit,
                    status:             Self::parse_order_status(&raw.status),
                    requested_size_usd: original,
                    filled_size_usd:    matched,
                    avg_fill_price:     if matched > 0.0 { Some(price) } else { None },
                    limit_price:        Some(price),
                    created_at:         Utc::now(),
                    updated_at:         Utc::now(),
                }
            })
            .collect())
    }

    async fn get_order_book(&self, market_id: &str) -> Result<ExchangeOrderBook, ExchangeError> {
        let url = format!("{}/book?token_id={}", self.clob_base, market_id);
        let resp = self.client.get(&url).send().await?;

        let raw: ClobOrderBookResponse = resp.json().await.map_err(|e| {
            ExchangeError::ParseError(format!("order book parse error: {e}"))
        })?;

        let bids = raw.bids.into_iter().map(|l| OrderBookLevel {
            price:    l.price.parse().unwrap_or(0.0),
            size_usd: l.size.parse().unwrap_or(0.0),
        }).collect();

        let asks = raw.asks.into_iter().map(|l| OrderBookLevel {
            price:    l.price.parse().unwrap_or(0.0),
            size_usd: l.size.parse().unwrap_or(0.0),
        }).collect();

        Ok(ExchangeOrderBook {
            market_id: market_id.to_string(),
            bids,
            asks,
            timestamp: Utc::now(),
        })
    }

    async fn get_recent_fills(&self, market_id: &str, limit: usize) -> Result<Vec<ExchangeFill>, ExchangeError> {
        let url = format!(
            "{}/trades?token_id={}&limit={}",
            self.clob_base, market_id, limit
        );
        let resp = self.client.get(&url).send().await?;

        let trades: Vec<ClobTradeResponse> = resp.json().await.map_err(|e| {
            ExchangeError::ParseError(format!("trades parse error: {e}"))
        })?;

        Ok(trades
            .into_iter()
            .map(|t| ExchangeFill {
                market_id: market_id.to_string(),
                price:     t.price.parse().unwrap_or(0.0),
                size_usd:  t.size.parse().unwrap_or(0.0),
                side:      Self::parse_side(&t.side),
                timestamp: Utc::now(), // TODO: parse t.created_at
            })
            .collect())
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
    fn parse_side_mapping() {
        assert_eq!(PolymarketConnector::parse_side("BUY"), OrderSide::BuyYes);
        assert_eq!(PolymarketConnector::parse_side("sell"), OrderSide::BuyNo);
        assert_eq!(PolymarketConnector::parse_side("NO"), OrderSide::BuyNo);
    }

    #[test]
    fn parse_status_mapping() {
        assert_eq!(PolymarketConnector::parse_order_status("live"), OrderStatus::Open);
        assert_eq!(PolymarketConnector::parse_order_status("matched"), OrderStatus::Filled);
        assert_eq!(PolymarketConnector::parse_order_status("cancelled"), OrderStatus::Cancelled);
    }
}
