// crates/execution_sim/src/engine.rs

use std::sync::Arc;

use chrono::Utc;
use common::{
    ApprovedTrade, EdgeDecayReport, Event, EventBus, ExecutionResult, Portfolio, PortfolioUpdate,
    Position,
};
use metrics::{counter, histogram};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    config::ExecutionConfig,
    state::SharedExecutionState,
    types::ExecutionOrder,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Box-Muller transform: returns one sample from N(0, `std_dev`).
///
/// `u1` must be in `(0, 1)` (exclusive of zero to avoid `ln(0)`).
/// `u2` can be in `[0, 1)`.
fn normal_sample<R: Rng>(rng: &mut R, std_dev: f64) -> f64 {
    let u1: f64 = rng.gen_range(f64::EPSILON..1.0_f64);
    let u2: f64 = rng.gen();
    let z = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
    z * std_dev
}

// ── ExecutionSimulator ────────────────────────────────────────────────────────

/// Simulated order execution engine.
///
/// # Pipeline position
///
/// ```text
/// RiskEngine
///       ↓  Event::ApprovedTrade
/// ExecutionSimulator          ← this module
///       ↓  Event::Execution(ExecutionResult)
///       ↓  Event::Portfolio(PortfolioUpdate)
/// PortfolioOptimizer / RiskEngine (feedback)
/// ```
///
/// Every approved trade passes through [`simulate_execution`], which applies
/// Gaussian slippage and probabilistic partial fills.  The resulting
/// `ExecutionResult` and an updated `PortfolioUpdate` are published on the bus.
///
/// No real orders are ever placed; this is the only place "trades" happen.
pub struct ExecutionSimulator {
    pub config: ExecutionConfig,
    /// Shared mutable state — exposed `pub` so tests can pre-seed or inspect.
    pub state: SharedExecutionState,
    bus: EventBus,
    rx: broadcast::Receiver<Arc<Event>>,
}

impl ExecutionSimulator {
    pub fn new(config: ExecutionConfig, bus: EventBus) -> Self {
        let mut state = crate::state::ExecutionState::default();
        state.bankroll = config.bankroll;
        state.peak_equity = config.bankroll;
        let rx = bus.subscribe();
        Self {
            config,
            state: Arc::new(tokio::sync::RwLock::new(state)),
            bus,
            rx,
        }
    }

    // ── Simulation core ───────────────────────────────────────────────────────

    /// Simulate execution of a single order, applying slippage and partial fills.
    ///
    /// This is a **pure function** of `config`, `order`, and `rng` — it has no
    /// side effects and can be called directly in unit tests with a seeded RNG.
    pub fn simulate_execution<R: Rng>(
        config: &ExecutionConfig,
        order: &ExecutionOrder,
        rng: &mut R,
        trade: ApprovedTrade,
    ) -> ExecutionResult {
        // ── Partial fill ──────────────────────────────────────────────────────
        let is_partial = rng.gen::<f64>() < config.partial_fill_probability;
        let fill_ratio: f64 = if is_partial {
            // Uniformly sample a fill between 50 % and 100 % of requested quantity.
            rng.gen_range(0.5_f64..1.0_f64)
        } else {
            1.0
        };
        let executed_quantity = order.quantity * fill_ratio;

        // ── Slippage ──────────────────────────────────────────────────────────
        let raw_slippage = if config.simulate_slippage {
            normal_sample(rng, config.slippage_std_dev)
        } else {
            0.0
        };

        // Cap slippage to max_slippage; reduce fill proportionally when exceeded.
        let (slippage, executed_quantity) = if raw_slippage.abs() > order.max_slippage {
            let scale = order.max_slippage / raw_slippage.abs();
            (raw_slippage.signum() * order.max_slippage, executed_quantity * scale)
        } else {
            (raw_slippage, executed_quantity)
        };

        // Recompute fill_ratio from the *final* executed_quantity so that
        // `fill_ratio × order.quantity == executed_quantity` always holds.
        // The slippage cap above may have further reduced executed_quantity beyond
        // the original partial-fill draw, making the earlier `fill_ratio` stale.
        let fill_ratio = if order.quantity > 0.0 {
            (executed_quantity / order.quantity).clamp(0.0, 1.0)
        } else {
            0.0
        };

        // avg_price must stay in (0, 1] for probability markets.
        let avg_price = (order.price * (1.0 + slippage)).clamp(f64::EPSILON, 1.0);

        let filled = executed_quantity > 0.0;

        ExecutionResult {
            trade,
            filled,
            fill_ratio,
            executed_quantity,
            avg_price,
            slippage,
            timestamp: Utc::now(),
        }
    }

    // ── Portfolio bookkeeping ─────────────────────────────────────────────────

    /// Apply a filled execution to the portfolio: upsert position and update exposure.
    ///
    /// When the same market is re-executed, the old position is replaced and any
    /// unrealised gain/loss from the prior position is accumulated into `pnl` as a
    /// *realised* component (using `trade.market_prob` as the exit price proxy).
    ///
    /// Full mark-to-market PnL across all open positions requires a current market
    /// price feed and is handled by a dedicated analytics module, not here.
    fn apply_fill(portfolio: &mut Portfolio, result: &ExecutionResult, bankroll: f64) {
        let trade = &result.trade;

        // If an existing position for this market is being replaced, realise its PnL.
        // We use market_prob (the current mid-price) as the exit price proxy.
        use common::TradeDirection;
        if let Some(old) = portfolio.positions.iter().find(|p| p.market_id == trade.market_id) {
            let realised = match old.direction {
                TradeDirection::Buy | TradeDirection::Arbitrage => {
                    (trade.market_prob - old.entry_probability) * old.size
                }
                TradeDirection::Sell => (old.entry_probability - trade.market_prob) * old.size,
            };
            portfolio.pnl += realised;
        }
        portfolio.positions.retain(|p| p.market_id != trade.market_id);

        portfolio.positions.push(Position {
            market_id:         trade.market_id.clone(),
            direction:         trade.direction,
            size:              result.executed_quantity * bankroll, // absolute USD
            entry_probability: result.avg_price,
            opened_at:         result.timestamp,
        });

        // Exposure = sum of all position sizes as fractions of bankroll.
        portfolio.exposure = portfolio.positions.iter().map(|p| p.size / bankroll).sum();
    }

    // ── Trade processing ──────────────────────────────────────────────────────

    async fn process_trade_parts(
        config: &ExecutionConfig,
        state:  &SharedExecutionState,
        bus:    &EventBus,
        rng:    &mut StdRng,
        trade:  &ApprovedTrade,
    ) {
        let order = ExecutionOrder {
            market_id:    trade.market_id.clone(),
            direction:    trade.direction,
            quantity:     trade.approved_fraction,
            price:        trade.market_prob,
            max_slippage: config.slippage_std_dev * 3.0, // 3-sigma cap
            timestamp:    trade.timestamp,
        };

        let result = Self::simulate_execution(config, &order, rng, trade.clone());

        // ── Latency tracking ──────────────────────────────────────────────────
        // Compute time from original signal emission to this execution fill.
        let execution_ts = result.timestamp;
        let latency_ms: u64 = execution_ts
            .signed_duration_since(trade.signal_timestamp)
            .num_milliseconds()
            .max(0) as u64;

        histogram!("execution_latency_ms").record(latency_ms as f64);

        debug!(
            market_id        = %trade.market_id,
            direction        = ?trade.direction,
            requested        = order.quantity,
            executed         = result.executed_quantity,
            avg_price        = result.avg_price,
            slippage         = result.slippage,
            fill_ratio       = result.fill_ratio,
            filled           = result.filled,
            latency_ms,
            "execution_sim: simulated trade"
        );

        // ── Update shared state ───────────────────────────────────────────────
        {
            let mut st = state.write().await;
            st.orders_processed += 1;
            if result.filled {
                st.orders_filled += 1;
                if result.fill_ratio < 1.0 {
                    st.orders_partial += 1;
                }
                st.total_slippage += result.slippage.abs();
                let bankroll = st.bankroll;
                Self::apply_fill(&mut st.portfolio, &result, bankroll);

                // Update peak equity (bankroll + realised PnL).
                let equity = st.bankroll + st.portfolio.pnl;
                if equity > st.peak_equity {
                    st.peak_equity = equity;
                }
            }
        }

        // ── Publish ExecutionResult ───────────────────────────────────────────
        counter!("execution_sim_orders_processed_total").increment(1);
        if result.filled {
            counter!("execution_sim_orders_filled_total").increment(1);
        }

        // Emit EdgeDecayReport only for filled trades — an unfilled trade has
        // no realized edge to track and would skew latency analytics.
        if result.filled {
            let decay_report = EdgeDecayReport {
                market_id:            trade.market_id.clone(),
                signal_expected_edge: trade.expected_value,
                execution_latency_ms: latency_ms,  // u64, clamped from signed duration
                signal_timestamp:     trade.signal_timestamp,
                execution_timestamp:  execution_ts,
            };
            if let Err(e) = bus.publish(Event::EdgeDecayReport(decay_report)) {
                warn!("execution_sim: failed to publish EdgeDecayReport: {e}");
            }
        }

        if let Err(e) = bus.publish(Event::Execution(result)) {
            warn!("execution_sim: failed to publish ExecutionResult: {e}");
        }

        // ── Publish PortfolioUpdate ───────────────────────────────────────────
        let portfolio_snapshot = {
            let st = state.read().await;
            st.portfolio.clone()
        };
        let update = PortfolioUpdate {
            portfolio: portfolio_snapshot,
            timestamp: Utc::now(),
        };
        if let Err(e) = bus.publish(Event::Portfolio(update)) {
            warn!("execution_sim: failed to publish PortfolioUpdate: {e}");
        }
    }

    // ── Main event loop ───────────────────────────────────────────────────────

    /// Subscribe to `Event::ApprovedTrade` and simulate execution for each one,
    /// publishing `Event::Execution` and `Event::Portfolio` on the bus.
    ///
    /// Runs until `cancel` fires or the bus closes.
    pub async fn run(self, cancel: CancellationToken) {
        let Self { config, state, bus, mut rx } = self;
        info!(
            simulate_slippage        = config.simulate_slippage,
            slippage_std_dev         = config.slippage_std_dev,
            partial_fill_probability = config.partial_fill_probability,
            bankroll                 = config.bankroll,
            "execution_sim: started — all trades are simulated only"
        );
        // Reconstruct a borrowable engine-like handle via a local helper closure
        // so we can call the existing process_trade logic without self.
        let mut rng = StdRng::from_entropy();
        loop {
            let event = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    info!("execution_sim: shutdown requested, exiting");
                    break;
                }
                result = rx.recv() => result,
            };

            match event {
                Ok(ev) => {
                    if let Event::ApprovedTrade(trade) = ev.as_ref() {
                        counter!("execution_sim_approved_trades_seen_total").increment(1);
                        Self::process_trade_parts(&config, &state, &bus, &mut rng, trade).await;
                    }
                }

                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("execution_sim: lagged by {n} events");
                }

                Err(broadcast::error::RecvError::Closed) => {
                    info!("execution_sim: event bus closed, shutting down");
                    break;
                }
            }
        }
    }
}
