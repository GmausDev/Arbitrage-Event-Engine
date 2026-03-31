// crates/oms/src/state.rs
//
// In-memory state for the Order Management System.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use common::TradeDirection;
use exchange_api::{ExchangeOrder, OrderId, OrderRequest, OrderStatus};
use serde::{Deserialize, Serialize};

/// An order tracked by the OMS with internal metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedOrder {
    /// The original order request submitted by the execution engine.
    pub request: OrderRequest,
    /// The trade direction from the original `ApprovedTrade`.
    pub direction: TradeDirection,
    /// Exchange name this order was sent to.
    pub exchange: String,
    /// Exchange-assigned order ID (set after successful submission).
    pub exchange_order_id: Option<OrderId>,
    /// Current lifecycle status.
    pub status: OrderStatus,
    /// Cumulative filled size in USD.
    pub filled_size_usd: f64,
    /// Volume-weighted average fill price.
    pub avg_fill_price: Option<f64>,
    /// Number of status polls performed for this order.
    pub poll_count: u32,
    /// When the order was submitted to the OMS.
    pub created_at: DateTime<Utc>,
    /// Last time the status was updated.
    pub updated_at: DateTime<Utc>,
    /// Internal OMS order ID (monotonically increasing).
    pub oms_id: u64,
}

impl ManagedOrder {
    /// True when the order is in a terminal state (no further updates expected).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            OrderStatus::Filled | OrderStatus::Cancelled | OrderStatus::Rejected | OrderStatus::Expired
        )
    }

    /// Fraction of the original order that has been filled [0.0, 1.0].
    pub fn fill_ratio(&self) -> f64 {
        if self.request.size_usd > 0.0 {
            (self.filled_size_usd / self.request.size_usd).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// Update from an exchange order snapshot.
    pub fn apply_exchange_update(&mut self, exchange_order: &ExchangeOrder) {
        self.status = exchange_order.status;
        self.filled_size_usd = exchange_order.filled_size_usd;
        self.avg_fill_price = exchange_order.avg_fill_price;
        self.updated_at = Utc::now();
        self.poll_count += 1;
    }
}

/// Aggregate OMS state.
#[derive(Debug, Default)]
pub struct OrderManagerState {
    /// Next OMS order ID.
    pub next_id: u64,
    /// All orders, indexed by OMS ID.
    pub orders: HashMap<u64, ManagedOrder>,
    /// Mapping from exchange order IDs to OMS IDs for lookup.
    pub exchange_to_oms: HashMap<String, u64>,
    /// Cached balance per exchange (updated on each balance check).
    pub balances: HashMap<String, f64>,

    // ── Aggregate counters ────────────────────────────────────────────────
    pub total_submitted: u64,
    pub total_filled: u64,
    pub total_rejected: u64,
    pub total_cancelled: u64,
    pub total_volume_usd: f64,
}

impl OrderManagerState {
    /// Register a new order.  Returns the OMS ID.
    pub fn insert_order(&mut self, request: OrderRequest, exchange: &str, direction: TradeDirection) -> u64 {
        let oms_id = self.next_id;
        self.next_id += 1;

        let managed = ManagedOrder {
            request,
            direction,
            exchange: exchange.to_string(),
            exchange_order_id: None,
            status: OrderStatus::Pending,
            filled_size_usd: 0.0,
            avg_fill_price: None,
            poll_count: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            oms_id,
        };

        self.orders.insert(oms_id, managed);
        self.total_submitted += 1;
        oms_id
    }

    /// Link an exchange order ID to an OMS order.
    pub fn set_exchange_id(&mut self, oms_id: u64, exchange_order_id: OrderId) {
        if let Some(order) = self.orders.get_mut(&oms_id) {
            self.exchange_to_oms
                .insert(exchange_order_id.0.clone(), oms_id);
            order.exchange_order_id = Some(exchange_order_id);
            order.status = OrderStatus::Open;
            order.updated_at = Utc::now();
        }
    }

    /// Mark an order as rejected (exchange refused it).
    pub fn mark_rejected(&mut self, oms_id: u64, reason: &str) {
        if let Some(order) = self.orders.get_mut(&oms_id) {
            order.status = OrderStatus::Rejected;
            order.updated_at = Utc::now();
            self.total_rejected += 1;
            tracing::warn!(
                oms_id,
                exchange = %order.exchange,
                market = %order.request.market_id,
                reason,
                "oms: order rejected"
            );
        }
    }

    /// Get all non-terminal orders that need polling.
    pub fn active_orders(&self) -> Vec<&ManagedOrder> {
        self.orders
            .values()
            .filter(|o| !o.is_terminal() && o.exchange_order_id.is_some())
            .collect()
    }

    /// Record a fill event (transition to Filled/PartiallyFilled).
    pub fn record_fill(&mut self, oms_id: u64, filled_usd: f64, avg_price: f64, fully_filled: bool) {
        if let Some(order) = self.orders.get_mut(&oms_id) {
            order.filled_size_usd = filled_usd;
            order.avg_fill_price = Some(avg_price);
            order.status = if fully_filled {
                OrderStatus::Filled
            } else {
                OrderStatus::PartiallyFilled
            };
            order.updated_at = Utc::now();

            if fully_filled {
                self.total_filled += 1;
                self.total_volume_usd += filled_usd;
            }
        }
    }
}
