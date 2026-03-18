// control-panel/src/event_collector.rs
//
// Subscribes to the shared EventBus, translates domain events into dashboard
// records, updates AppState ring buffers, and broadcasts WsEvent messages to
// all connected WebSocket clients.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use common::{Event, EventBus, TradeDirection};

use crate::state::{
    ring_push, AppState, EquityPoint, ExecutionRecord, MarketRecord, PortfolioSnapshot,
    PositionRecord, ShockRecord, SignalRecord, StrategyRecord, WsEvent, EQUITY_RING, EXEC_RING,
    SHOCK_RING, SIGNAL_RING, STRAT_RING,
};

pub struct EventCollector;

impl EventCollector {
    /// Spawn the collector as a background task.  Returns immediately.
    pub fn spawn(
        bus:    EventBus,
        state:  Arc<AppState>,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            Self::run(bus, state, cancel).await;
        })
    }

    async fn run(bus: EventBus, state: Arc<AppState>, cancel: CancellationToken) {
        let mut rx = bus.subscribe();
        info!("control_panel: event collector started");

        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    info!("control_panel: event collector shutting down");
                    break;
                }
                result = rx.recv() => {
                    match result {
                        Ok(ev) => {
                            state.event_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            Self::handle(&state, ev.as_ref()).await;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!(n, "control_panel: event collector lagged");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            info!("control_panel: bus closed");
                            break;
                        }
                    }
                }
            }
        }
    }

    async fn handle(state: &AppState, event: &Event) {
        match event {
            // ── Raw market price tick ─────────────────────────────────────
            Event::Market(u) => {
                let rec = MarketRecord {
                    market_id:   u.market.id.clone(),
                    probability: u.market.probability,
                    liquidity:   u.market.liquidity,
                    bid:         u.market.probability * 0.99, // placeholder until snapshot
                    ask:         (u.market.probability * 1.01).min(1.0),
                    volume:      0.0,
                    last_update: u.market.last_update,
                };
                state.markets.write().await.insert(rec.market_id.clone(), rec.clone());
                state.broadcast(WsEvent::MarketUpdate {
                    market_id:   rec.market_id,
                    probability: rec.probability,
                    liquidity:   rec.liquidity,
                    timestamp:   rec.last_update,
                });
            }

            // ── Rich market snapshot from data_ingestion layer ────────────
            Event::MarketSnapshot(s) => {
                let rec = MarketRecord {
                    market_id:   s.market_id.clone(),
                    probability: s.probability,
                    liquidity:   s.liquidity,
                    bid:         s.bid,
                    ask:         s.ask,
                    volume:      s.volume,
                    last_update: s.timestamp,
                };
                state.markets.write().await.insert(rec.market_id.clone(), rec.clone());
                state.broadcast(WsEvent::MarketUpdate {
                    market_id:   rec.market_id,
                    probability: rec.probability,
                    liquidity:   rec.liquidity,
                    timestamp:   rec.last_update,
                });
            }

            // ── Trade signals (all strategy agents) ──────────────────────
            Event::Signal(sig) => {
                let rec = SignalRecord {
                    timestamp:         sig.timestamp,
                    agent:             sig.source.clone(),
                    market_id:         sig.market_id.clone(),
                    model_probability: sig.posterior_prob,
                    market_price:      sig.market_prob,
                    edge:              sig.expected_value,
                    direction:         format!("{:?}", sig.direction),
                    confidence:        sig.confidence,
                    expected_value:    sig.expected_value,
                };
                {
                    let mut buf = state.signals.write().await;
                    ring_push(&mut buf, rec.clone(), SIGNAL_RING);
                }
                state.broadcast(WsEvent::Signal(rec));
            }

            // ── Meta-signal (fused across strategy agents) ────────────────
            Event::MetaSignal(ms) => {
                state.broadcast(WsEvent::MetaSignal {
                    market_id:     ms.market_id.clone(),
                    direction:     format!("{:?}", ms.direction),
                    confidence:    ms.confidence,
                    expected_edge: ms.expected_edge,
                    timestamp:     ms.timestamp,
                });
                debug!(
                    market_id = %ms.market_id,
                    confidence = ms.confidence,
                    "control_panel: MetaSignal"
                );
            }

            // ── Execution results ─────────────────────────────────────────
            Event::Execution(ex) => {
                let rec = ExecutionRecord {
                    timestamp:         ex.timestamp,
                    market_id:         ex.trade.market_id.clone(),
                    direction:         format!("{:?}", ex.trade.direction),
                    approved_fraction: ex.trade.approved_fraction,
                    fill_ratio:        ex.fill_ratio,
                    executed_quantity: ex.executed_quantity,
                    avg_price:         ex.avg_price,
                    slippage:          ex.slippage,
                    filled:            ex.filled,
                };
                {
                    let mut buf = state.executions.write().await;
                    ring_push(&mut buf, rec.clone(), EXEC_RING);
                }
                state.broadcast(WsEvent::Execution(rec));
            }

            // ── Portfolio updates ─────────────────────────────────────────
            Event::Portfolio(pu) => {
                let total_pnl = pu.portfolio.pnl;
                let equity    = state.initial_bankroll + total_pnl;

                // ── MTM: realized / unrealized split ─────────────────────────
                // Mirrors performance_analytics::position_mtm using the current
                // market price from our markets map.
                let unrealized_pnl = {
                    let markets = state.markets.read().await;
                    pu.portfolio.positions.iter().fold(0.0_f64, |acc, pos| {
                        match markets.get(&pos.market_id) {
                            Some(mrec) => acc + match pos.direction {
                                TradeDirection::Buy | TradeDirection::Arbitrage =>
                                    (mrec.probability - pos.entry_probability) * pos.size,
                                TradeDirection::Sell =>
                                    (pos.entry_probability - mrec.probability) * pos.size,
                            },
                            None => acc,
                        }
                    })
                };
                let realized_pnl = total_pnl - unrealized_pnl;

                // ── Position records ──────────────────────────────────────────
                let positions: Vec<PositionRecord> = pu
                    .portfolio
                    .positions
                    .iter()
                    .map(|p| PositionRecord {
                        market_id:         p.market_id.clone(),
                        direction:         format!("{:?}", p.direction),
                        size:              p.size,
                        entry_probability: p.entry_probability,
                        opened_at:         p.opened_at,
                    })
                    .collect();

                // ── Append to equity curve ────────────────────────────────────
                {
                    let mut curve = state.equity_curve.write().await;
                    ring_push(
                        &mut curve,
                        EquityPoint { timestamp: pu.timestamp, equity, pnl: total_pnl },
                        EQUITY_RING,
                    );
                }

                // ── Sharpe + win rate from curve (matches performance_analytics) ─
                let (sharpe, win_rate) = {
                    let curve = state.equity_curve.read().await;
                    compute_sharpe_winrate(&curve)
                };

                // ── Global HWM drawdown (never decreases) ─────────────────────
                let cur_dd = {
                    let mut peak = state.peak_equity.write().await;
                    if equity > *peak { *peak = equity; }
                    let p = *peak;
                    if p.is_finite() && p > 0.0 { ((p - equity) / p).max(0.0) } else { 0.0 }
                };
                let max_dd = {
                    let mut hwm = state.max_drawdown_hwm.write().await;
                    if cur_dd > *hwm { *hwm = cur_dd; }
                    *hwm
                };

                let snap = PortfolioSnapshot {
                    equity,
                    pnl:              total_pnl,
                    realized_pnl,
                    unrealized_pnl,
                    exposure:         pu.portfolio.exposure,
                    positions:        positions.clone(),
                    sharpe,
                    max_drawdown:     max_dd,
                    current_drawdown: cur_dd,
                    win_rate,
                    position_count:   positions.len(),
                    last_update:      pu.timestamp,
                };

                *state.portfolio.write().await = snap.clone();
                state.broadcast(WsEvent::Portfolio(snap));
            }

            // ── Shocks ────────────────────────────────────────────────────
            Event::Shock(sh) => {
                let rec = ShockRecord {
                    timestamp: sh.timestamp,
                    market_id: sh.market_id.clone(),
                    magnitude: sh.magnitude,
                    direction: format!("{:?}", sh.direction),
                    source:    format!("{:?}", sh.source),
                    z_score:   sh.z_score,
                };
                {
                    let mut buf = state.shocks.write().await;
                    ring_push(&mut buf, rec.clone(), SHOCK_RING);
                }
                state.broadcast(WsEvent::Shock(rec));
            }

            // ── Strategy research promotions ──────────────────────────────
            Event::StrategyDiscovered(sd) => {
                let rec = StrategyRecord {
                    strategy_id:  sd.strategy_id.clone(),
                    sharpe:       sd.sharpe,
                    max_drawdown: sd.max_drawdown,
                    win_rate:     sd.win_rate,
                    trade_count:  sd.trade_count,
                    promoted_at:  sd.promoted_at,
                };
                {
                    let mut buf = state.strategies.write().await;
                    ring_push(&mut buf, rec.clone(), STRAT_RING);
                }
                state.broadcast(WsEvent::StrategyDiscovered(rec));
            }

            // ── All other events are not surfaced to the dashboard ────────
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Sharpe + win rate — aligned with performance_analytics formulas
// ---------------------------------------------------------------------------

/// Compute annualised Sharpe ratio and win rate from the equity curve.
///
/// Formula matches `performance_analytics::compute_risk_metrics`:
/// - Returns: **absolute PnL changes** per tick (`curr.pnl − prev.pnl`)
/// - Std dev: **sample variance** (divides by n−1)
/// - Annualisation: `sqrt(365.25 × 24 × 3600 / 60)` ≈ 725 (60-second tick default)
///
/// Drawdown and peak tracking are handled separately via `AppState::peak_equity`
/// and `AppState::max_drawdown_hwm` (global high-water marks, never decrease).
fn compute_sharpe_winrate(curve: &std::collections::VecDeque<EquityPoint>) -> (f64, f64) {
    if curve.len() < 2 {
        return (0.0, 0.0);
    }

    // Absolute PnL changes (matches performance_analytics — not percentage returns)
    let returns: Vec<f64> = curve
        .iter()
        .zip(curve.iter().skip(1))
        .map(|(prev, curr)| curr.pnl - prev.pnl)
        .collect();

    let n    = returns.len() as f64;
    let mean = returns.iter().sum::<f64>() / n;
    // Sample variance (n-1) — matches performance_analytics
    let var  = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0).max(1.0);
    let std  = var.sqrt();

    // Ann factor: sqrt(31_557_600 / 60) ≈ 725 — matches AnalyticsConfig default
    const ANN_FACTOR: f64 = 725.23;
    let sharpe = if std > f64::EPSILON { mean / std * ANN_FACTOR } else { 0.0 };

    let wins     = returns.iter().filter(|&&r| r > 0.0).count();
    let win_rate = wins as f64 / n;

    (sharpe, win_rate)
}
