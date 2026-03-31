// crates/persistence/src/reconciliation.rs
//
// AccountReconciler — periodically queries exchange connectors for balances
// and positions, compares against internal OMS/portfolio state, and logs
// discrepancies as AuditEntry records.

use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use tracing::{error, info, warn};

use exchange_api::ExchangeConnector;

use crate::db::Database;
use crate::models::{AuditEntry, ReconciliationResult};

/// Periodically reconciles internal state against exchange-reported balances
/// and positions.  Discrepancies are logged to the audit trail.
pub struct AccountReconciler {
    db: Arc<Database>,
    exchanges: Vec<Arc<dyn ExchangeConnector>>,
    /// How often to reconcile, in seconds.
    interval_secs: u64,
}

impl AccountReconciler {
    pub fn new(db: Arc<Database>, interval_secs: u64) -> Self {
        Self {
            db,
            exchanges: Vec::new(),
            interval_secs,
        }
    }

    /// Register an exchange connector for reconciliation.
    pub fn with_exchange(mut self, connector: Arc<dyn ExchangeConnector>) -> Self {
        self.exchanges.push(connector);
        self
    }

    /// Run the reconciliation loop.  Checks all registered exchanges at the
    /// configured interval.  Runs until cancelled.  Takes ownership of self
    /// so the future is `'static` and can be passed to `tokio::spawn`.
    pub async fn run(
        self,
        internal_balances: Arc<tokio::sync::RwLock<std::collections::HashMap<String, f64>>>,
    ) -> Result<()> {
        info!(
            "persistence: AccountReconciler started (interval={}s, exchanges={})",
            self.interval_secs,
            self.exchanges.len(),
        );

        if self.exchanges.is_empty() {
            info!("persistence: no exchanges registered for reconciliation, sleeping");
            // Still run the loop so the task stays alive (exchanges could be
            // added dynamically in a future version).
        }

        let mut interval = tokio::time::interval(
            tokio::time::Duration::from_secs(self.interval_secs),
        );

        loop {
            interval.tick().await;

            for exchange in &self.exchanges {
                if let Err(e) = self
                    .reconcile_exchange(exchange.as_ref(), &internal_balances)
                    .await
                {
                    error!(
                        "persistence: reconciliation failed for {}: {e}",
                        exchange.name()
                    );
                    // Log the failure itself as an audit event.
                    let _ = self.db.insert_audit(&AuditEntry {
                        id: None,
                        category: "reconciliation".into(),
                        summary: format!(
                            "Reconciliation failed for {}: {e}",
                            exchange.name()
                        ),
                        details_json: "{}".into(),
                        timestamp: Utc::now(),
                    });
                }
            }
        }
    }

    /// Reconcile a single exchange: compare balance and positions.
    async fn reconcile_exchange(
        &self,
        exchange: &dyn ExchangeConnector,
        internal_balances: &tokio::sync::RwLock<std::collections::HashMap<String, f64>>,
    ) -> Result<()> {
        let name = exchange.name().to_string();

        // ── Balance check ───────────────────────────────────────────────────
        let exchange_balance = exchange.get_balance().await.map_err(|e| {
            anyhow::anyhow!("get_balance failed: {e}")
        })?;

        let internal_balance = {
            let balances = internal_balances.read().await;
            balances.get(&name).copied().unwrap_or(0.0)
        };

        let balance_diff = (exchange_balance - internal_balance).abs();

        // ── Position check ──────────────────────────────────────────────────
        let exchange_positions = exchange.get_positions().await.map_err(|e| {
            anyhow::anyhow!("get_positions failed: {e}")
        })?;

        // For now, we just count positions and log them.  A full reconciliation
        // would compare each position against the internal portfolio — that
        // requires access to the PortfolioEngine's state, which we can add
        // when the portfolio crate exposes a query API.
        let mut mismatch_details = Vec::new();

        if balance_diff > 1.0 {
            // More than $1 discrepancy is noteworthy.
            mismatch_details.push(format!(
                "Balance mismatch: internal={:.2} exchange={:.2} diff={:.2}",
                internal_balance, exchange_balance, balance_diff,
            ));
        }

        let result = ReconciliationResult {
            exchange: name.clone(),
            internal_balance,
            exchange_balance,
            balance_diff,
            position_mismatches: mismatch_details.len(),
            mismatch_details: mismatch_details.clone(),
            timestamp: Utc::now(),
        };

        // Log to audit trail.
        let has_issues = !mismatch_details.is_empty();
        let summary = if has_issues {
            format!(
                "Reconciliation {}: {} issue(s) — balance diff=${:.2}",
                name,
                mismatch_details.len(),
                balance_diff,
            )
        } else {
            format!(
                "Reconciliation {}: OK (balance={:.2}, {} positions)",
                name, exchange_balance, exchange_positions.len(),
            )
        };

        let details = serde_json::json!({
            "exchange": name,
            "internal_balance": internal_balance,
            "exchange_balance": exchange_balance,
            "balance_diff": balance_diff,
            "exchange_positions": exchange_positions.len(),
            "mismatches": mismatch_details,
        });

        self.db.insert_audit(&AuditEntry {
            id: None,
            category: "reconciliation".into(),
            summary,
            details_json: details.to_string(),
            timestamp: result.timestamp,
        })?;

        if has_issues {
            warn!(
                "persistence: reconciliation {} found {} issue(s)",
                name,
                mismatch_details.len(),
            );
            metrics::counter!("persistence_reconciliation_mismatches_total")
                .increment(mismatch_details.len() as u64);
        } else {
            info!("persistence: reconciliation {} OK", name);
        }

        metrics::counter!("persistence_reconciliation_checks_total").increment(1);
        Ok(())
    }
}
