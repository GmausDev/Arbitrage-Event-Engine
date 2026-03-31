// crates/persistence/src/recorder.rs
//
// EventRecorder — subscribes to the EventBus and automatically persists:
//   - ExecutionResult  → TradeRecord (with full P&L attribution)
//   - PortfolioUpdate  → EquitySnapshot (periodic equity curve)
//   - TradeRejected    → AuditEntry (audit trail)

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use common::event_bus::EventBus;
use common::events::{Event, ExecutionResult, TradeRejected};
use common::types::Portfolio;
use cost_model::config::CostModelConfig;
use cost_model::model::{compute_cost_estimate_for_exchange, compute_expected_profit, compute_net_edge};

use crate::db::Database;
use crate::models::{AuditEntry, EquitySnapshot, TradeRecord};

/// Subscribes to the event bus and persists execution results, portfolio
/// snapshots, and trade rejections to the SQLite database.
pub struct EventRecorder {
    db: Arc<Database>,
    cost_config: CostModelConfig,
    bankroll: f64,
}

impl EventRecorder {
    pub fn new(db: Arc<Database>, cost_config: CostModelConfig, bankroll: f64) -> Self {
        Self { db, cost_config, bankroll }
    }

    /// Spawn the recorder loop as a tokio task.
    ///
    /// Listens on the event bus until it closes, persisting relevant events.
    /// Returns `Ok(())` when the bus shuts down.
    pub async fn run(&self, event_bus: &EventBus) -> Result<()> {
        let mut rx = event_bus.subscribe();
        info!("persistence: EventRecorder started");

        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let Err(e) = self.handle_event(event.as_ref()) {
                        error!("persistence: failed to record event: {e}");
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("persistence: event bus lagged, skipped {n} events");
                    metrics::counter!("persistence_events_lagged_total").increment(n);
                }
                Err(broadcast::error::RecvError::Closed) => {
                    info!("persistence: event bus closed, recorder shutting down");
                    break;
                }
            }
        }

        Ok(())
    }

    fn handle_event(&self, event: &Event) -> Result<()> {
        match event {
            Event::Execution(exec) => self.record_execution(exec),
            Event::Portfolio(update) => self.record_portfolio(&update.portfolio, update.timestamp),
            Event::TradeRejected(rej) => self.record_rejection(rej),
            _ => Ok(()),
        }
    }

    // ── Execution → TradeRecord ─────────────────────────────────────────────

    fn record_execution(&self, exec: &ExecutionResult) -> Result<()> {
        let trade = &exec.trade;
        // Determine exchange from market_id heuristic (same as OMS routing).
        let exchange = detect_exchange(&trade.market_id);

        // Compute P&L attribution.
        let gross_edge = (trade.posterior_prob - trade.market_prob).abs();
        let position_fraction = trade.approved_fraction * exec.fill_ratio;
        let latency_ms = (exec.timestamp - trade.signal_timestamp)
            .num_milliseconds()
            .max(0) as f64;

        let cost = compute_cost_estimate_for_exchange(
            gross_edge,
            position_fraction,
            self.cost_config.default_volatility,
            latency_ms,
            &exchange,
            &self.cost_config,
        );

        let net_edge = compute_net_edge(gross_edge, &cost);
        let filled_size_usd = position_fraction * self.bankroll;
        let expected_profit = compute_expected_profit(net_edge, filled_size_usd);

        let side = format!("{:?}", trade.direction).to_lowercase();

        let record = TradeRecord {
            id: None,
            exchange: exchange.clone(),
            order_id: format!("evt-{}", exec.timestamp.timestamp_millis()),
            market_id: trade.market_id.clone(),
            side,
            signal_source: trade.signal_source.clone(),

            requested_size_usd: trade.approved_fraction * self.bankroll,
            filled_size_usd,
            fill_ratio: exec.fill_ratio,

            market_price: trade.market_prob,
            posterior_prob: trade.posterior_prob,
            avg_fill_price: exec.avg_price,

            gross_edge,
            fee_cost: cost.fees,
            spread_cost: cost.spread,
            slippage_cost: cost.slippage,
            decay_cost: cost.decay_cost,
            total_cost: cost.total_cost,
            net_edge,
            realized_slippage: exec.slippage,
            expected_profit_usd: expected_profit,

            status: if exec.filled { "filled".into() } else { "unfilled".into() },
            signal_timestamp: trade.signal_timestamp,
            submitted_at: trade.timestamp,
            filled_at: exec.timestamp,
            latency_ms: latency_ms as i64,
        };

        let id = self.db.insert_trade(&record)?;
        debug!(
            "persistence: recorded trade #{id} market={} edge={:.4} net={:.4}",
            trade.market_id, gross_edge, net_edge,
        );
        metrics::counter!("persistence_trades_recorded_total").increment(1);
        Ok(())
    }

    // ── PortfolioUpdate → EquitySnapshot ────────────────────────────────────

    fn record_portfolio(
        &self,
        portfolio: &Portfolio,
        timestamp: chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        let cash = self.bankroll - portfolio.exposure;
        let equity = cash + portfolio.pnl + portfolio.exposure;

        // We don't have a running peak tracker here — use equity as a
        // conservative estimate; the query layer can compute the true HWM
        // from the stored equity curve.
        let snapshot = EquitySnapshot {
            id: None,
            equity_usd: equity,
            cash_usd: cash,
            deployed_usd: portfolio.exposure,
            open_positions: portfolio.positions.len() as i64,
            realized_pnl: portfolio.pnl,
            unrealized_pnl: 0.0, // Portfolio.pnl is combined; split at query time
            peak_equity: equity,
            drawdown: 0.0,
            timestamp,
        };

        self.db.insert_equity_snapshot(&snapshot)?;
        debug!(
            "persistence: equity snapshot equity={:.2} positions={}",
            equity,
            portfolio.positions.len(),
        );
        metrics::counter!("persistence_snapshots_recorded_total").increment(1);
        Ok(())
    }

    // ── TradeRejected → AuditEntry ──────────────────────────────────────────

    fn record_rejection(&self, rej: &TradeRejected) -> Result<()> {
        let details = serde_json::json!({
            "market_id": rej.market_id,
            "reason": rej.reason,
            "expected_edge": rej.expected_edge,
            "signal_source": rej.signal_source,
        });

        let entry = AuditEntry {
            id: None,
            category: "rejection".into(),
            summary: format!(
                "Rejected {} (source={}, edge={:.4}): {}",
                rej.market_id, rej.signal_source, rej.expected_edge, rej.reason,
            ),
            details_json: details.to_string(),
            timestamp: rej.timestamp,
        };

        self.db.insert_audit(&entry)?;
        debug!("persistence: recorded rejection for {}", rej.market_id);
        metrics::counter!("persistence_rejections_recorded_total").increment(1);
        Ok(())
    }
}

/// Heuristic to detect exchange from market_id format.
/// Kalshi tickers are uppercase with hyphens/underscores; Polymarket IDs
/// are mixed-case hex or long alphanumeric strings.
fn detect_exchange(market_id: &str) -> String {
    let is_kalshi = market_id.len() < 30
        && market_id
            .chars()
            .all(|c| c.is_ascii_uppercase() || c == '-' || c == '_' || c.is_ascii_digit());
    if is_kalshi { "kalshi".into() } else { "polymarket".into() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use common::events::{ApprovedTrade, ExecutionResult, TradeRejected};
    use common::types::TradeDirection;

    fn test_db() -> Arc<Database> {
        Arc::new(Database::open(":memory:").unwrap())
    }

    fn default_cost_config() -> CostModelConfig {
        CostModelConfig::default()
    }

    fn sample_execution() -> ExecutionResult {
        let now = Utc::now();
        ExecutionResult {
            trade: ApprovedTrade {
                market_id: "AAPL-2024-YES".into(),
                direction: TradeDirection::Buy,
                approved_fraction: 0.05,
                expected_value: 0.03,
                posterior_prob: 0.65,
                market_prob: 0.55,
                signal_source: "signal_agent".into(),
                signal_timestamp: now - chrono::Duration::milliseconds(100),
                timestamp: now - chrono::Duration::milliseconds(50),
            },
            filled: true,
            fill_ratio: 0.9,
            executed_quantity: 0.045,
            avg_price: 0.56,
            slippage: 0.018,
            timestamp: now,
        }
    }

    #[test]
    fn test_record_execution() {
        let db = test_db();
        let recorder = EventRecorder::new(db.clone(), default_cost_config(), 10_000.0);
        let exec = sample_execution();

        recorder.record_execution(&exec).unwrap();

        let trades = db.get_recent_trades(10).unwrap();
        assert_eq!(trades.len(), 1);
        let t = &trades[0];
        assert_eq!(t.exchange, "kalshi"); // Uppercase ticker → kalshi
        assert_eq!(t.market_id, "AAPL-2024-YES");
        assert_eq!(t.status, "filled");
        assert!(t.gross_edge > 0.0);
        assert!(t.latency_ms >= 0);
    }

    #[test]
    fn test_record_rejection() {
        let db = test_db();
        let recorder = EventRecorder::new(db.clone(), default_cost_config(), 10_000.0);

        let rej = TradeRejected {
            market_id: "AAPL-2024-YES".into(),
            reason: "exposure limit exceeded".into(),
            expected_edge: 0.02,
            signal_source: "signal_agent".into(),
            timestamp: Utc::now(),
        };

        recorder.record_rejection(&rej).unwrap();

        let audits = db.get_audit_log(Some("rejection"), 10).unwrap();
        assert_eq!(audits.len(), 1);
        assert!(audits[0].summary.contains("exposure limit"));
    }

    #[test]
    fn test_detect_exchange() {
        assert_eq!(detect_exchange("AAPL-2024-YES"), "kalshi");
        assert_eq!(detect_exchange("BTC-50K"), "kalshi");
        assert_eq!(
            detect_exchange("0x1234abcd5678ef901234567890abcdef12345678"),
            "polymarket"
        );
        assert_eq!(detect_exchange("some-mixed-Case-id"), "polymarket");
    }
}
