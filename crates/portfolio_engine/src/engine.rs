// crates/portfolio_engine/src/engine.rs

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use common::{
    Event, EventBus, ExecutionResult, Portfolio, PortfolioUpdate, Position, TradeDirection,
};
use metrics::counter;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    config::PortfolioConfig,
    state::{PortfolioState, SharedPortfolioState},
    types::PortfolioPosition,
};

// ── PortfolioEngine ───────────────────────────────────────────────────────────

/// Authoritative portfolio tracker.
///
/// # Pipeline position
///
/// ```text
/// ExecutionSimulator
///       ↓  Event::Execution(ExecutionResult)
/// PortfolioEngine                ← this module
///       ↓  Event::Portfolio(PortfolioUpdate)
/// RiskEngine / PerformanceAnalytics
/// ```
///
/// `PortfolioEngine` is the single source of truth for position state.  It
/// maintains a richer internal representation than `execution_sim`'s basic
/// tracking: per-position realised and unrealised PnL, last observed market
/// price, and optional strategy tags.
///
/// All mutating operations (`apply_execution`, `recalculate_unrealized`,
/// `apply_risk_adjustments`) are **pure functions** that take `&mut
/// PortfolioState` — no async, no locks — so they can be called directly in
/// unit tests with constructed state.
pub struct PortfolioEngine {
    pub config: PortfolioConfig,
    /// Shared mutable state — `pub` so tests can pre-seed or inspect.
    pub state:  SharedPortfolioState,
    bus:        EventBus,
    rx:         broadcast::Receiver<Arc<Event>>,
}

impl PortfolioEngine {
    pub fn new(config: PortfolioConfig, bus: EventBus) -> Self {
        if let Err(e) = config.validate() {
            panic!("invalid PortfolioConfig: {e}");
        }
        let state = PortfolioState::new(config.initial_bankroll);
        let rx = bus.subscribe();
        Self {
            config,
            state: Arc::new(tokio::sync::RwLock::new(state)),
            bus,
            rx,
        }
    }

    // ── Pure core logic ───────────────────────────────────────────────────────

    /// Apply a filled `ExecutionResult` to `state`.
    ///
    /// **Position replacement**: if a position already exists for the same
    /// market the old position is closed at `trade.market_prob` (used as an
    /// exit-price proxy), its realised PnL crystallised, and its deployed
    /// capital returned to `cash`.  The new position then occupies its slot.
    ///
    /// Unfilled results (`result.filled == false`) are silently ignored.
    pub fn apply_execution(state: &mut PortfolioState, result: &ExecutionResult) {
        if !result.filled {
            return;
        }

        let trade    = &result.trade;
        let bankroll = state.initial_bankroll;
        let new_size = result.executed_quantity * bankroll; // convert fraction → USD

        // ── Close any existing position for this market ───────────────────────
        if let Some(old) = state.positions.remove(&trade.market_id) {
            // Crystallise the gain/loss vs. the current mid-price.
            let realised = match old.direction {
                TradeDirection::Buy | TradeDirection::Arbitrage => {
                    (trade.market_prob - old.avg_price) * old.size
                }
                TradeDirection::Sell => (old.avg_price - trade.market_prob) * old.size,
            };
            state.total_realized_pnl   += old.realized_pnl + realised;
            state.total_unrealized_pnl -= old.unrealized_pnl;
            state.cash                 += old.size; // return deployed capital
        }

        // ── Open the new position ─────────────────────────────────────────────
        // Clamp to available cash so we never deploy money we don't have.
        let new_size = new_size.min(state.cash.max(0.0));
        if new_size <= 0.0 {
            warn!(
                market_id = %trade.market_id,
                requested = result.executed_quantity * bankroll,
                cash      = state.cash,
                "portfolio_engine: insufficient cash to open position, skipping"
            );
            return;
        }
        state.cash -= new_size;
        state.positions.insert(
            trade.market_id.clone(),
            PortfolioPosition {
                market_id:         trade.market_id.clone(),
                direction:         trade.direction,
                size:              new_size,
                avg_price:         result.avg_price,
                realized_pnl:      0.0,
                unrealized_pnl:    0.0,
                last_market_price: Some(trade.market_prob),
                opened_at:         result.timestamp,
                tags:              Vec::new(),
            },
        );

        state.last_update = Utc::now();

        debug!(
            market_id  = %trade.market_id,
            direction  = ?trade.direction,
            size_usd   = new_size,
            avg_price  = result.avg_price,
            cash       = state.cash,
            real_pnl   = state.total_realized_pnl,
            "portfolio_engine: position updated"
        );
    }

    /// Recompute unrealised PnL for every open position given fresh market prices.
    ///
    /// `market_prices` maps `market_id → current_probability`.  Positions
    /// whose `market_id` is absent from the map keep their previous
    /// `unrealized_pnl` value unchanged.
    pub fn recalculate_unrealized(
        state:         &mut PortfolioState,
        market_prices: &HashMap<String, f64>,
    ) {
        let mut total = 0.0_f64;
        for pos in state.positions.values_mut() {
            if let Some(&price) = market_prices.get(&pos.market_id) {
                pos.last_market_price = Some(price);
                pos.unrealized_pnl = match pos.direction {
                    TradeDirection::Buy | TradeDirection::Arbitrage => {
                        (price - pos.avg_price) * pos.size
                    }
                    TradeDirection::Sell => (pos.avg_price - price) * pos.size,
                };
            }
            total += pos.unrealized_pnl;
        }
        state.total_unrealized_pnl = total;
        state.last_update          = Utc::now();
    }

    /// Enforce per-position size limits from `max_position_fraction`.
    ///
    /// Any position whose size exceeds `initial_bankroll × max_position_fraction`
    /// is trimmed in-place and the excess returned to `cash`.  In a live system
    /// this would generate trim orders; here we clamp and log.
    pub fn apply_risk_adjustments(state: &mut PortfolioState, max_fraction: f64) {
        let max_size = state.initial_bankroll * max_fraction;
        for pos in state.positions.values_mut() {
            if pos.size > max_size + f64::EPSILON {
                let excess = pos.size - max_size;
                warn!(
                    market_id = %pos.market_id,
                    size      = pos.size,
                    max_size  = max_size,
                    excess    = excess,
                    "portfolio_engine: position exceeds cap — trimming"
                );
                pos.size  = max_size;
                state.cash += excess;
            }
        }
        state.last_update = Utc::now();
    }

    // ── Snapshot helpers ──────────────────────────────────────────────────────

    /// Return the shared state handle so callers can read the current snapshot.
    pub fn get_portfolio_state(&self) -> SharedPortfolioState {
        Arc::clone(&self.state)
    }

    /// Retrieve a copy of a specific position, if it exists.
    pub async fn get_position(&self, market_id: &str) -> Option<PortfolioPosition> {
        self.state.read().await.positions.get(market_id).cloned()
    }

    /// Convert internal `PortfolioState` into the `common::PortfolioUpdate`
    /// event that is broadcast on the Event Bus.
    ///
    /// `common::Portfolio` is the wire-type used by all downstream consumers;
    /// this function bridges the richer internal representation to it.
    fn to_portfolio_update(state: &PortfolioState) -> PortfolioUpdate {
        let positions: Vec<Position> = state
            .positions
            .values()
            .map(|p| Position {
                market_id:         p.market_id.clone(),
                direction:         p.direction,
                size:              p.size,
                entry_probability: p.avg_price,
                opened_at:         p.opened_at,
            })
            .collect();

        let portfolio = Portfolio {
            positions,
            // Surface total net PnL (realised + unrealised) on the wire type.
            pnl:      state.total_realized_pnl + state.total_unrealized_pnl,
            exposure: state.total_deployed(),
        };

        PortfolioUpdate {
            portfolio,
            timestamp: Utc::now(),
        }
    }

    // ── Event processing ──────────────────────────────────────────────────────

    async fn process_execution(&self, result: &ExecutionResult) {
        // Drop unfilled results immediately — no state change, no event.
        if !result.filled {
            return;
        }

        // Apply fill and risk adjustments, then snapshot for publishing — all
        // under a single write-lock to avoid a second lock acquisition.
        let update = {
            let mut state = self.state.write().await;
            Self::apply_execution(&mut state, result);
            Self::apply_risk_adjustments(&mut state, self.config.max_position_fraction);
            Self::to_portfolio_update(&state)
        };

        counter!("portfolio_engine_executions_applied_total").increment(1);
        counter!("portfolio_engine_positions_filled_total").increment(1);

        if let Err(e) = self.bus.publish(Event::Portfolio(update)) {
            warn!("portfolio_engine: failed to publish PortfolioUpdate: {e}");
        }
    }

    // ── Main event loop ───────────────────────────────────────────────────────

    /// Subscribe to `Event::Execution` and apply each fill to the portfolio,
    /// publishing `Event::Portfolio` after every update.
    ///
    /// Runs until `cancel` fires or the bus closes.
    pub async fn run(self, cancel: CancellationToken) {
        let Self { config, state, bus, mut rx } = self;
        info!(
            initial_bankroll      = config.initial_bankroll,
            max_position_fraction = config.max_position_fraction,
            "portfolio_engine: started"
        );

        loop {
            let event = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    info!("portfolio_engine: shutdown requested, exiting");
                    break;
                }
                result = rx.recv() => result,
            };

            match event {
                Ok(ev) => match ev.as_ref() {
                    Event::Execution(result) => {
                        counter!("portfolio_engine_executions_seen_total").increment(1);
                        // Apply fill and risk adjustments under one write-lock.
                        let update = {
                            let mut st = state.write().await;
                            Self::apply_execution(&mut st, result);
                            Self::apply_risk_adjustments(&mut st, config.max_position_fraction);
                            Self::to_portfolio_update(&st)
                        };
                        if result.filled {
                            counter!("portfolio_engine_executions_applied_total").increment(1);
                            counter!("portfolio_engine_positions_filled_total").increment(1);
                            if let Err(e) = bus.publish(Event::Portfolio(update)) {
                                warn!("portfolio_engine: failed to publish PortfolioUpdate: {e}");
                            }
                        }
                    }
                    Event::Market(update) => {
                        // Keep unrealised PnL current as market prices arrive.
                        let prices = HashMap::from([(
                            update.market.id.clone(),
                            update.market.probability,
                        )]);
                        let mut st = state.write().await;
                        Self::recalculate_unrealized(&mut st, &prices);
                    }
                    _ => {}
                },

                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("portfolio_engine: lagged by {n} events");
                }

                Err(broadcast::error::RecvError::Closed) => {
                    info!("portfolio_engine: event bus closed, shutting down");
                    break;
                }
            }
        }
    }
}
