// crates/risk_engine/src/engine.rs

use std::sync::Arc;

use chrono::Utc;
use tokio::sync::RwLock;
use common::{ApprovedTrade, Event, EventBus, FastSignal, OptimizedSignal, TradeRejected, TradeSignal};
use metrics::{counter, gauge};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    config::RiskConfig,
    state::{Position, RiskState, SharedRiskState},
    types::RejectionReason,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Extract the cluster prefix from a market ID.
///
/// `"US_ELECTION_TRUMP"` → `"US"`;  `"BTCUSD"` → `"BTCUSD"`.
#[inline]
fn cluster_id(market_id: &str) -> &str {
    market_id.split('_').next().unwrap_or(market_id)
}

// ── RiskEngine ────────────────────────────────────────────────────────────────

pub struct RiskEngine {
    pub config: RiskConfig,
    /// Shared mutable state; accessible from tests via `Arc::clone(&engine.state)`.
    pub state: SharedRiskState,
    bus: EventBus,
    rx: Option<broadcast::Receiver<Arc<Event>>>,
}

impl RiskEngine {
    pub fn new(config: RiskConfig, bus: EventBus) -> Self {
        let rx = bus.subscribe();
        let state = {
            let mut s = RiskState::default();
            s.bankroll = config.bankroll;
            s.peak_equity = config.bankroll;
            s.current_equity = config.bankroll;
            Arc::new(RwLock::new(s))
        };
        Self {
            config,
            state,
            bus,
            rx: Some(rx),
        }
    }

    // ── Trade evaluation pipeline ─────────────────────────────────────────────

    /// Evaluate a `TradeSignal` through the full risk pipeline.
    ///
    /// All reads *and* mutations happen inside a single write-lock acquisition.
    /// No `.await` point exists while the lock is held — the only `.await` is
    /// the lock acquisition itself.
    ///
    /// Returns `Ok(ApprovedTrade)` if every gate passes, or `Err(RejectionReason)`.
    async fn evaluate(
        &self,
        signal: &TradeSignal,
    ) -> Result<ApprovedTrade, (RejectionReason, TradeRejected)> {
        let mut state = self.state.write().await;
        // No .await beyond this point until the guard drops.

        // Helper: build a TradeRejected payload for missed-edge tracking.
        let make_rejected = |reason: RejectionReason| -> TradeRejected {
            TradeRejected {
                market_id:     signal.market_id.clone(),
                reason:        reason.to_string(),  // RejectionReason: Copy, passed by value
                expected_edge: signal.expected_value,
                signal_source: signal.source.clone(),
                timestamp:     Utc::now(),
            }
        };

        // Step 1 — count the signal.
        state.signals_seen += 1;

        // Step 2 — Expected value gate.
        if signal.expected_value < self.config.min_expected_value {
            state.trades_rejected += 1;
            let r = RejectionReason::LowExpectedValue;
            return Err((r, make_rejected(r)));
        }

        // Step 3 — Drawdown protection.
        let drawdown = if state.peak_equity > 0.0 {
            (state.peak_equity - state.current_equity) / state.peak_equity
        } else {
            0.0
        };
        if drawdown > self.config.max_drawdown {
            state.trades_rejected += 1;
            let r = RejectionReason::DrawdownProtection;
            return Err((r, make_rejected(r)));
        }

        // Step 3.5 — Release any existing position for this market before checking
        // exposure limits.  Without this, re-evaluating a market where we already
        // hold a position would double-count the old size in total_exposure and
        // cluster_exposure, causing spurious rejections or incorrect resizing.
        let cluster = cluster_id(&signal.market_id);
        if let Some(old) = state.positions.get(&signal.market_id) {
            let old_size = old.size_fraction;
            state.total_exposure = (state.total_exposure - old_size).max(0.0);
            let entry = state.cluster_exposure.entry(cluster.to_string()).or_insert(0.0);
            *entry = (*entry - old_size).max(0.0);
        }

        // Step 4 — Position size cap.
        let mut approved = signal.position_fraction.min(self.config.max_position_fraction);

        // Step 5 — Total exposure check (resize or reject).
        let remaining_total = self.config.max_total_exposure - state.total_exposure;
        if remaining_total <= 0.0 {
            state.trades_rejected += 1;
            let r = RejectionReason::TotalExposureFull;
            return Err((r, make_rejected(r)));
        }
        approved = approved.min(remaining_total);

        // Step 6 — Cluster exposure check (resize or reject).
        let used = state.cluster_exposure.get(cluster).copied().unwrap_or(0.0);
        let remaining_cluster = self.config.max_cluster_exposure - used;
        if remaining_cluster <= 0.0 {
            state.trades_rejected += 1;
            let r = RejectionReason::ClusterExposureFull;
            return Err((r, make_rejected(r)));
        }
        approved = approved.min(remaining_cluster);

        // Step 7 — Zero-size guard.
        if approved <= 0.0 {
            state.trades_rejected += 1;
            let r = RejectionReason::ZeroApprovedSize;
            return Err((r, make_rejected(r)));
        }

        // Step 8 — Approve: update all exposure bookkeeping.
        state.total_exposure += approved;
        *state.cluster_exposure.entry(cluster.to_string()).or_insert(0.0) += approved;
        state.positions.insert(
            signal.market_id.clone(),
            Position {
                market_id:     signal.market_id.clone(),
                size_fraction: approved,
                entry_price:   signal.market_prob,
                direction:     signal.direction,
            },
        );
        state.trades_approved += 1;

        Ok(ApprovedTrade {
            market_id:         signal.market_id.clone(),
            direction:         signal.direction,
            approved_fraction: approved,
            expected_value:    signal.expected_value,
            posterior_prob:    signal.posterior_prob,
            market_prob:       signal.market_prob,
            signal_timestamp:  signal.timestamp,
            timestamp:         Utc::now(),
        })
    }

    /// Helper shared by the raw-signal, fast-signal, and optimized-signal branches.
    async fn process_signal(&self, signal: &TradeSignal) {
        counter!("risk_signals_seen_total").increment(1);
        match self.evaluate(signal).await {
            Ok(approved) => {
                counter!("risk_trades_approved_total").increment(1);
                debug!(
                    market_id         = %approved.market_id,
                    approved_fraction = approved.approved_fraction,
                    ev                = approved.expected_value,
                    "risk_engine: signal approved"
                );
                if let Err(e) = self.bus.publish(Event::ApprovedTrade(approved)) {
                    warn!("risk_engine: failed to publish ApprovedTrade: {e}");
                }
            }
            Err((reason, rejected)) => {
                counter!("risk_trades_rejected_total").increment(1);
                // Missed-edge tracking: increment counter by the expected_value
                // of the rejected signal so operators can observe opportunity cost.
                let ev = rejected.expected_edge;
                if ev.is_finite() && ev > 0.0 {
                    counter!("risk_missed_edge_total").increment(1);
                    gauge!("risk_missed_edge_last").set(ev);
                }
                debug!(
                    market_id = %signal.market_id,
                    reason    = %reason,
                    ev        = ev,
                    "risk_engine: signal rejected"
                );
                if let Err(e) = self.bus.publish(Event::TradeRejected(rejected)) {
                    warn!("risk_engine: failed to publish TradeRejected: {e}");
                }
            }
        }
    }

    // ── Main event loop ───────────────────────────────────────────────────────

    /// Subscribe to `Event::Signal` events and process each through the risk
    /// pipeline, publishing `Event::ApprovedTrade` for accepted signals.
    ///
    /// Runs until `cancel` fires or the bus closes.
    pub async fn run(mut self, cancel: CancellationToken) {
        let mut rx = self.rx.take().expect("rx already consumed");
        info!("risk_engine: started — listening for Signal events");

        loop {
            let event = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    info!("risk_engine: shutdown requested, exiting");
                    break;
                }
                result = rx.recv() => result,
            };

            match event {
                Ok(ev) => match ev.as_ref() {
                    // Raw signal — only when `accept_raw_signals` is enabled (no priority engine).
                    Event::Signal(signal) if self.config.accept_raw_signals => {
                        self.process_signal(signal).await;
                    }

                    // Fast signal from SignalPriorityEngine — always evaluated immediately,
                    // regardless of `accept_raw_signals`.  These bypass the optimizer batch window.
                    Event::FastSignal(FastSignal { signal, priority_score }) => {
                        counter!("risk_fast_signals_seen_total").increment(1);
                        debug!(
                            market_id      = %signal.market_id,
                            priority_score,
                            "risk_engine: fast signal received"
                        );
                        self.process_signal(signal).await;
                    }

                    // Optimized signal from PortfolioOptimizer — portfolio-aware allocation.
                    Event::OptimizedSignal(OptimizedSignal { signal, .. }) => {
                        self.process_signal(signal).await;
                    }
                    _ => {}
                },

                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("risk_engine: lagged by {n} events");
                }

                Err(broadcast::error::RecvError::Closed) => {
                    info!("risk_engine: event bus closed, shutting down");
                    break;
                }
            }
        }
    }
}
