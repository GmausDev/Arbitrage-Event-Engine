// crates/risk_engine/src/engine.rs

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::sync::RwLock;
use common::{ApprovedTrade, Event, EventBus, FastSignal, OptimizedSignal, TradeRejected, TradeSignal};
use cost_model::{compute_cost_estimate, compute_expected_profit, compute_net_edge, compute_net_edge_position_size};
use metrics::{counter, gauge};
use tokio::sync::broadcast;
use tokio::time::interval;
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

        // Step 2.5 — Cost model: net edge must be positive and dollar profit
        // must meet the minimum threshold.  This gate runs after the raw EV
        // check but before exposure bookkeeping so we never mutate state for
        // economically unviable signals.
        let gross_edge = (signal.posterior_prob - signal.market_prob).abs();
        let volatility = self.config.cost.default_volatility;
        let latency_ms = state.avg_latency_ms;
        let cost = compute_cost_estimate(
            gross_edge,
            signal.position_fraction,
            volatility,
            latency_ms,
            &self.config.cost,
        );
        let net_edge = compute_net_edge(gross_edge, &cost);

        // Emit per-trade cost metrics.
        gauge!("risk_cost_fees_last").set(cost.fees);
        gauge!("risk_cost_spread_last").set(cost.spread);
        gauge!("risk_cost_slippage_last").set(cost.slippage);
        gauge!("risk_cost_decay_last").set(cost.decay_cost);
        gauge!("risk_net_edge_last").set(net_edge);
        gauge!("risk_edge_efficiency_last").set(if gross_edge > 0.0 {
            net_edge / gross_edge
        } else {
            0.0
        });

        // Rolling diagnostic accumulators.
        state.gross_edge_sum  += gross_edge;
        state.net_edge_sum    += net_edge;
        state.total_cost_sum  += cost.total_cost;
        state.cost_trades_count += 1;

        if net_edge <= 0.0 {
            state.trades_rejected += 1;
            state.trades_rejected_negative_edge += 1;
            counter!("risk_negative_edge_rejections_total").increment(1);
            let r = RejectionReason::NegativeNetEdge;
            return Err((r, make_rejected(r)));
        }

        let position_size_usd = signal.position_fraction * state.bankroll;
        let expected_profit   = compute_expected_profit(net_edge, position_size_usd);
        if expected_profit < self.config.cost.min_expected_profit_usd {
            state.trades_rejected += 1;
            state.trades_rejected_insufficient_profit += 1;
            counter!("risk_insufficient_profit_rejections_total").increment(1);
            let r = RejectionReason::InsufficientExpectedProfit;
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
        //
        // `released_size` is captured so we can roll back the subtraction if a
        // later gate rejects the signal.  The positions map entry is NOT removed
        // here — it is only replaced on a successful approval (Step 8).
        let cluster = cluster_id(&signal.market_id);
        let released_size: f64 = if let Some(old) = state.positions.get(&signal.market_id) {
            let sz = old.size_fraction;
            state.total_exposure = (state.total_exposure - sz).max(0.0);
            let e = state.cluster_exposure.entry(cluster.to_string()).or_insert(0.0);
            *e = (*e - sz).max(0.0);
            sz
        } else {
            0.0
        };

        // Inline rollback used by Steps 5, 6, and 7 on rejection.
        // Restores total_exposure and cluster_exposure to pre-3.5 values so the
        // old position's capital is not silently lost from the accounting.
        macro_rules! rollback_release {
            ($state:expr) => {
                if released_size > 0.0 {
                    $state.total_exposure += released_size;
                    *$state.cluster_exposure.entry(cluster.to_string()).or_insert(0.0) += released_size;
                }
            };
        }

        // Step 4 — Position size cap.
        let mut approved = signal.position_fraction.min(self.config.max_position_fraction);

        // Step 5 — Total exposure check (resize or reject).
        let remaining_total = self.config.max_total_exposure - state.total_exposure;
        if remaining_total <= 0.0 {
            rollback_release!(state);
            state.trades_rejected += 1;
            let r = RejectionReason::TotalExposureFull;
            return Err((r, make_rejected(r)));
        }
        approved = approved.min(remaining_total);

        // Step 6 — Cluster exposure check (resize or reject).
        let used = state.cluster_exposure.get(cluster).copied().unwrap_or(0.0);
        let remaining_cluster = self.config.max_cluster_exposure - used;
        if remaining_cluster <= 0.0 {
            rollback_release!(state);
            state.trades_rejected += 1;
            let r = RejectionReason::ClusterExposureFull;
            return Err((r, make_rejected(r)));
        }
        approved = approved.min(remaining_cluster);

        // Step 6.5 — Scale position fraction by net-to-gross edge ratio so
        // Kelly sizing reflects realised net edge rather than raw gross edge.
        approved = compute_net_edge_position_size(approved, net_edge, gross_edge);

        // Step 7 — Zero-size guard.
        if approved <= 0.0 {
            rollback_release!(state);
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
            signal_source:     signal.source.clone(),
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

        // Evict market_liquidity entries for markets no longer actively broadcasting.
        // A seen-set is accumulated between ticks; anything absent at tick time is dropped.
        let mut liquidity_eviction_tick = interval(Duration::from_secs(300));
        liquidity_eviction_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut seen_liquidity_ids: HashSet<String> = HashSet::new();

        loop {
            let event = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    info!("risk_engine: shutdown requested, exiting");
                    break;
                }
                _ = liquidity_eviction_tick.tick() => {
                    let mut state = self.state.write().await;
                    let before = state.market_liquidity.len();
                    state.market_liquidity.retain(|k, _| seen_liquidity_ids.contains(k));
                    let evicted = before.saturating_sub(state.market_liquidity.len());
                    if evicted > 0 {
                        info!(evicted, remaining = state.market_liquidity.len(),
                            "risk_engine: evicted stale liquidity entries");
                    }
                    seen_liquidity_ids.clear();
                    continue;
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

                    // Keep exposure state in sync with the authoritative portfolio.
                    // This prevents the risk engine's internal position map from
                    // drifting (e.g., after TTL-based position expiry).
                    Event::Portfolio(pu) => {
                        let mut state = self.state.write().await;
                        // Sync equity and drawdown watermark.
                        let equity = self.config.bankroll + pu.portfolio.pnl;
                        state.current_equity = equity;
                        if equity > state.peak_equity {
                            state.peak_equity = equity;
                        }
                        // Rebuild position map and exposure counters from actual portfolio.
                        state.positions.clear();
                        state.cluster_exposure.clear();
                        state.total_exposure = 0.0;
                        for pos in &pu.portfolio.positions {
                            let fraction = (pos.size / self.config.bankroll)
                                .min(self.config.max_position_fraction);
                            let cl = cluster_id(&pos.market_id).to_string();
                            state.positions.insert(pos.market_id.clone(), Position {
                                market_id:     pos.market_id.clone(),
                                size_fraction: fraction,
                                entry_price:   pos.entry_probability,
                                direction:     pos.direction,
                            });
                            state.total_exposure += fraction;
                            *state.cluster_exposure.entry(cl).or_insert(0.0) += fraction;
                        }
                        debug!(
                            total_exposure = state.total_exposure,
                            positions      = state.positions.len(),
                            equity,
                            "risk_engine: synced from portfolio update"
                        );
                    }
                    // Cache per-market liquidity for the cost model.
                    Event::Market(update) => {
                        seen_liquidity_ids.insert(update.market.id.clone());
                        if update.market.liquidity > 0.0 {
                            let mut state = self.state.write().await;
                            state.market_liquidity.insert(
                                update.market.id.clone(),
                                update.market.liquidity,
                            );
                        }
                    }

                    // Update EMA of observed execution latency for decay estimation.
                    Event::EdgeDecayReport(report) => {
                        const ALPHA: f64 = 0.1;
                        let observed = report.execution_latency_ms as f64;
                        let mut state = self.state.write().await;
                        state.avg_latency_ms =
                            (1.0 - ALPHA) * state.avg_latency_ms + ALPHA * observed;
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
