// crates/oms/src/manager.rs
//
// Core order management logic.  Bridges ApprovedTrade events from the risk
// engine to exchange connectors, then polls for fill status and publishes
// Event::Execution results back on the event bus.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use common::{ApprovedTrade, EdgeDecayReport, Event, EventBus, ExecutionResult};
use exchange_api::{ExchangeConnector, OrderId, OrderRequest, OrderSide, OrderStatus, OrderType};
use metrics::{counter, gauge, histogram};
use tokio::sync::broadcast;
use tokio::sync::RwLock;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::state::OrderManagerState;

// ── Configuration ────────────────────────────────────────────────────────────

/// OMS runtime configuration.
#[derive(Debug, Clone)]
pub struct OmsConfig {
    /// How often to poll exchanges for order status updates (ms).
    pub poll_interval_ms: u64,
    /// Maximum age of a pending/open order before auto-cancellation (seconds).
    pub order_timeout_secs: u64,
    /// Whether to actually place orders (false = dry-run / log-only mode).
    pub live_trading_enabled: bool,
    /// Reference bankroll for converting fractions to USD amounts.
    pub bankroll: f64,
}

impl Default for OmsConfig {
    fn default() -> Self {
        Self {
            poll_interval_ms:     2_000,
            order_timeout_secs:   300, // 5 minutes
            live_trading_enabled: false,
            bankroll:             10_000.0,
        }
    }
}

// ── OrderManager ─────────────────────────────────────────────────────────────

/// The Order Manager subscribes to `Event::ApprovedTrade`, places orders on
/// the appropriate exchange, polls for fills, and publishes `Event::Execution`.
pub struct OrderManager {
    config: OmsConfig,
    bus: EventBus,
    rx: Option<broadcast::Receiver<Arc<Event>>>,
    state: Arc<RwLock<OrderManagerState>>,
    /// Registered exchange connectors, keyed by exchange name.
    exchanges: HashMap<String, Arc<dyn ExchangeConnector>>,
}

impl OrderManager {
    pub fn new(config: OmsConfig, bus: EventBus) -> Self {
        let rx = bus.subscribe();
        Self {
            config,
            bus,
            rx: Some(rx),
            state: Arc::new(RwLock::new(OrderManagerState::default())),
            exchanges: HashMap::new(),
        }
    }

    /// Register an exchange connector.  The OMS will route orders to the
    /// appropriate exchange based on market ID prefixes or explicit mapping.
    pub fn with_exchange(mut self, connector: Arc<dyn ExchangeConnector>) -> Self {
        self.exchanges.insert(connector.name().to_string(), connector);
        self
    }

    /// Access the shared OMS state (for dashboards, tests, etc.).
    pub fn state(&self) -> Arc<RwLock<OrderManagerState>> {
        self.state.clone()
    }

    // ── Order submission ─────────────────────────────────────────────────────

    /// Convert an `ApprovedTrade` into an `OrderRequest` and submit it.
    async fn submit_trade(&self, trade: &ApprovedTrade) -> Option<()> {
        // Determine which exchange to route to.
        // Convention: market IDs starting with "KALSHI-" or matching Kalshi ticker
        // patterns go to Kalshi; everything else goes to Polymarket.
        let exchange_name = if trade.market_id.starts_with("KALSHI-")
            || trade.market_id.chars().all(|c| c.is_uppercase() || c == '-' || c == '_')
        {
            "kalshi"
        } else {
            "polymarket"
        };

        let connector = match self.exchanges.get(exchange_name) {
            Some(c) => c.clone(),
            None => {
                warn!(
                    market = %trade.market_id,
                    exchange = exchange_name,
                    "oms: no connector registered for exchange, skipping"
                );
                return None;
            }
        };

        let size_usd = trade.approved_fraction * self.config.bankroll;
        if size_usd < connector.min_order_size_usd() {
            debug!(
                market = %trade.market_id,
                size_usd,
                min = connector.min_order_size_usd(),
                "oms: order below minimum size, skipping"
            );
            return None;
        }

        let order_request = OrderRequest {
            market_id:        trade.market_id.clone(),
            side:             OrderSide::from(trade.direction),
            order_type:       OrderType::Limit,
            size_usd,
            price:            Some(trade.market_prob),
            signal_source:    trade.signal_source.clone(),
            signal_timestamp: trade.signal_timestamp,
        };

        // Register in OMS state.
        let oms_id = {
            let mut state = self.state.write().await;
            state.insert_order(order_request.clone(), exchange_name, trade.direction)
        };

        if !self.config.live_trading_enabled {
            info!(
                oms_id,
                exchange = exchange_name,
                market   = %trade.market_id,
                size_usd = format!("{:.2}", size_usd),
                price    = format!("{:.4}", trade.market_prob),
                "oms: DRY RUN — order logged but NOT sent to exchange"
            );
            // In dry-run mode, immediately synthesize a fill for pipeline continuity.
            self.synthesize_fill(oms_id, trade, size_usd).await;
            return Some(());
        }

        // Place the order on the exchange.
        match connector.place_order(&order_request).await {
            Ok(exchange_id) => {
                let mut state = self.state.write().await;
                state.set_exchange_id(oms_id, exchange_id.clone());
                counter!("oms_orders_submitted_total", "exchange" => exchange_name.to_string()).increment(1);
                info!(
                    oms_id,
                    exchange_id = %exchange_id,
                    exchange    = exchange_name,
                    market      = %trade.market_id,
                    size_usd    = format!("{:.2}", size_usd),
                    "oms: order submitted to exchange"
                );
            }
            Err(e) => {
                let mut state = self.state.write().await;
                state.mark_rejected(oms_id, &e.to_string());
                counter!("oms_orders_rejected_total", "exchange" => exchange_name.to_string()).increment(1);
                warn!(
                    oms_id,
                    exchange = exchange_name,
                    market   = %trade.market_id,
                    err      = %e,
                    "oms: order submission failed"
                );

                // Publish a zero-fill ExecutionResult so downstream knows this trade failed.
                let exec_result = ExecutionResult {
                    trade: trade.clone(),
                    filled: false,
                    fill_ratio: 0.0,
                    executed_quantity: 0.0,
                    avg_price: trade.market_prob,
                    slippage: 0.0,
                    timestamp: Utc::now(),
                };
                let _ = self.bus.publish(Event::Execution(exec_result));
            }
        }

        Some(())
    }

    /// In dry-run mode, create a synthetic fill matching the approved trade.
    async fn synthesize_fill(&self, oms_id: u64, trade: &ApprovedTrade, size_usd: f64) {
        let mut state = self.state.write().await;
        state.record_fill(oms_id, size_usd, trade.market_prob, true);

        let exec_result = ExecutionResult {
            trade:             trade.clone(),
            filled:            true,
            fill_ratio:        1.0,
            executed_quantity:  trade.approved_fraction,
            avg_price:         trade.market_prob,
            slippage:          0.0,
            timestamp:         Utc::now(),
        };

        let latency_ms = exec_result.timestamp
            .signed_duration_since(trade.signal_timestamp)
            .num_milliseconds()
            .max(0) as u64;

        let decay_report = EdgeDecayReport {
            market_id:            trade.market_id.clone(),
            signal_expected_edge: trade.expected_value,
            execution_latency_ms: latency_ms,
            signal_timestamp:     trade.signal_timestamp,
            execution_timestamp:  exec_result.timestamp,
        };

        let _ = self.bus.publish(Event::EdgeDecayReport(decay_report));
        let _ = self.bus.publish(Event::Execution(exec_result));
    }

    // ── Order polling ────────────────────────────────────────────────────────

    /// Poll all active orders for status updates.
    async fn poll_active_orders(&self) {
        let active: Vec<(u64, String, OrderId)> = {
            let state = self.state.read().await;
            state.active_orders().iter().filter_map(|o| {
                let eid = o.exchange_order_id.as_ref()?;
                Some((o.oms_id, o.exchange.clone(), eid.clone()))
            }).collect()
        };

        for (oms_id, exchange_name, exchange_id) in active {
            let connector = match self.exchanges.get(&exchange_name) {
                Some(c) => c.clone(),
                None => continue,
            };

            match connector.get_order_status(&exchange_id).await {
                Ok(exchange_order) => {
                    let prev_filled = {
                        let state = self.state.read().await;
                        state.orders.get(&oms_id).map(|o| o.filled_size_usd).unwrap_or(0.0)
                    };

                    let exec_result = {
                        let mut state = self.state.write().await;
                        if let Some(order) = state.orders.get_mut(&oms_id) {
                            order.apply_exchange_update(&exchange_order);
                        }

                        // If there's new fill volume, build an ExecutionResult.
                        if exchange_order.filled_size_usd > prev_filled {
                            let fully_filled = exchange_order.status == OrderStatus::Filled;

                            // Snapshot order data before record_fill borrows state mutably.
                            let order_snapshot = state.orders.get(&oms_id).cloned();

                            state.record_fill(
                                oms_id,
                                exchange_order.filled_size_usd,
                                exchange_order.avg_fill_price.unwrap_or(0.0),
                                fully_filled,
                            );

                            counter!("oms_fills_total", "exchange" => exchange_name.clone()).increment(1);
                            histogram!("oms_fill_size_usd", "exchange" => exchange_name.clone())
                                .record(exchange_order.filled_size_usd);

                            order_snapshot.map(|order| ExecutionResult {
                                trade: ApprovedTrade {
                                    market_id:         order.request.market_id.clone(),
                                    direction:         order.direction,
                                    approved_fraction: order.request.size_usd / self.config.bankroll,
                                    expected_value:    0.0,
                                    posterior_prob:     0.0,
                                    market_prob:       order.request.price.unwrap_or(0.0),
                                    signal_source:     order.request.signal_source.clone(),
                                    signal_timestamp:  order.request.signal_timestamp,
                                    timestamp:         order.created_at,
                                },
                                filled:            exchange_order.filled_size_usd > 0.0,
                                fill_ratio:        exchange_order.fill_ratio(),
                                executed_quantity:  exchange_order.filled_size_usd / self.config.bankroll,
                                avg_price:         exchange_order.avg_fill_price.unwrap_or(0.0),
                                slippage:          0.0,
                                timestamp:         Utc::now(),
                            })
                        } else {
                            None
                        }
                    };

                    if let Some(exec_result) = exec_result {
                        let _ = self.bus.publish(Event::Execution(exec_result));
                    }
                }
                Err(e) => {
                    debug!(
                        oms_id,
                        exchange = %exchange_name,
                        err = %e,
                        "oms: order status poll failed"
                    );
                }
            }
        }
    }

    /// Cancel orders that have been open longer than the timeout.
    async fn cancel_stale_orders(&self) {
        let now = Utc::now();
        let timeout = chrono::Duration::seconds(self.config.order_timeout_secs as i64);

        let stale: Vec<(u64, String, OrderId)> = {
            let state = self.state.read().await;
            state.active_orders().iter().filter_map(|o| {
                let eid = o.exchange_order_id.as_ref()?;
                if now.signed_duration_since(o.created_at) > timeout {
                    Some((o.oms_id, o.exchange.clone(), eid.clone()))
                } else {
                    None
                }
            }).collect()
        };

        for (oms_id, exchange_name, exchange_id) in stale {
            if let Some(connector) = self.exchanges.get(&exchange_name) {
                match connector.cancel_order(&exchange_id).await {
                    Ok(result) => {
                        if result.success {
                            let mut state = self.state.write().await;
                            if let Some(order) = state.orders.get_mut(&oms_id) {
                                order.status = OrderStatus::Cancelled;
                                order.updated_at = Utc::now();
                                state.total_cancelled += 1;
                            }
                            info!(oms_id, exchange_id = %exchange_id, "oms: stale order cancelled");
                            counter!("oms_orders_cancelled_total", "exchange" => exchange_name).increment(1);
                        }
                    }
                    Err(e) => {
                        warn!(oms_id, err = %e, "oms: failed to cancel stale order");
                    }
                }
            }
        }
    }

    // ── Main event loop ──────────────────────────────────────────────────────

    /// Run the OMS event loop.
    pub async fn run(mut self, cancel: CancellationToken) {
        let mut rx = self.rx.take().expect("rx already consumed");

        info!(
            live = self.config.live_trading_enabled,
            poll_ms = self.config.poll_interval_ms,
            exchanges = ?self.exchanges.keys().collect::<Vec<_>>(),
            "oms: started"
        );

        let mut poll_tick = interval(Duration::from_millis(self.config.poll_interval_ms));
        poll_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Stale-order check every 60 seconds.
        let mut stale_tick = interval(Duration::from_secs(60));
        stale_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Metrics emission every 10 seconds.
        let mut metrics_tick = interval(Duration::from_secs(10));
        metrics_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                biased;

                _ = cancel.cancelled() => {
                    info!("oms: shutdown requested");
                    break;
                }

                // Poll exchanges for order updates.
                _ = poll_tick.tick() => {
                    if self.config.live_trading_enabled {
                        self.poll_active_orders().await;
                    }
                }

                // Cancel stale orders.
                _ = stale_tick.tick() => {
                    if self.config.live_trading_enabled {
                        self.cancel_stale_orders().await;
                    }
                }

                // Emit aggregate metrics.
                _ = metrics_tick.tick() => {
                    let state = self.state.read().await;
                    gauge!("oms_active_orders").set(state.active_orders().len() as f64);
                    gauge!("oms_total_submitted").set(state.total_submitted as f64);
                    gauge!("oms_total_filled").set(state.total_filled as f64);
                    gauge!("oms_total_volume_usd").set(state.total_volume_usd);
                }

                // Process incoming events.
                result = rx.recv() => {
                    match result {
                        Ok(ev) => {
                            if let Event::ApprovedTrade(trade) = ev.as_ref() {
                                counter!("oms_approved_trades_seen_total").increment(1);
                                self.submit_trade(trade).await;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("oms: lagged by {n} events");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("oms: event bus closed, shutting down");
                            break;
                        }
                    }
                }
            }
        }

        // Log final stats.
        let state = self.state.read().await;
        info!(
            submitted = state.total_submitted,
            filled    = state.total_filled,
            rejected  = state.total_rejected,
            cancelled = state.total_cancelled,
            volume    = format!("${:.2}", state.total_volume_usd),
            "oms: shutdown complete"
        );
    }
}
