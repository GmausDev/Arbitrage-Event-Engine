// crates/portfolio_optimizer/src/engine.rs

use std::sync::Arc;
use std::time::Duration;

use common::{Event, EventBus, TradeSignal};
use metrics::counter;
use tokio::sync::broadcast;
use tokio::time;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    allocator::{AllocationInput, compute_allocations},
    config::AllocationConfig,
    state::{SharedOptimizerState, new_shared_state},
};

// ── PortfolioOptimizer ────────────────────────────────────────────────────────

/// Batch portfolio optimizer.
///
/// Accumulates `Event::Signal` events over a configurable tick window, runs
/// the allocation algorithm, then publishes one `Event::OptimizedSignal` per
/// surviving allocation before clearing the batch.
///
/// # Position in the pipeline
///
/// ```text
/// SignalAgent / ArbAgent
///       ↓  Event::Signal
/// PortfolioOptimizer          ← this module
///       ↓  Event::OptimizedSignal
/// RiskEngine
///       ↓  Event::ApprovedTrade
/// ExecutionSim
/// ```
pub struct PortfolioOptimizer {
    pub config: AllocationConfig,
    /// Shared mutable state — exposed `pub` so tests can pre-seed or inspect.
    pub state:  SharedOptimizerState,
    bus:        EventBus,
    rx:         Option<broadcast::Receiver<Arc<Event>>>,
}

impl PortfolioOptimizer {
    pub fn new(config: AllocationConfig, bus: EventBus) -> Self {
        let rx = bus.subscribe();
        Self { config, state: new_shared_state(), bus, rx: Some(rx) }
    }

    // ── Main event loop ───────────────────────────────────────────────────────

    /// Listen for `Event::Signal` and `Event::Portfolio`, accumulate signals,
    /// and flush optimized allocations every `config.tick_ms` milliseconds.
    ///
    /// Runs until `cancel` is triggered or the event bus closes.
    pub async fn run(mut self, cancel: CancellationToken) {
        let mut rx = self.rx.take().expect("rx already consumed");
        let mut tick = time::interval(Duration::from_millis(self.config.tick_ms));
        // Skip missed ticks to avoid bursts after a slow flush.
        tick.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

        info!(
            tick_ms = self.config.tick_ms,
            max_per_market = self.config.max_allocation_per_market,
            max_per_niche  = self.config.max_allocation_per_niche,
            "portfolio_optimizer: started"
        );

        loop {
            tokio::select! {
                biased;

                _ = cancel.cancelled() => {
                    info!("portfolio_optimizer: shutdown requested, flushing final batch");
                    self.flush_batch().await;
                    break;
                }

                result = rx.recv() => {
                    match result {
                        Ok(ev) => self.handle_event(ev.as_ref()).await,

                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("portfolio_optimizer: lagged by {n} events");
                        }

                        Err(broadcast::error::RecvError::Closed) => {
                            info!("portfolio_optimizer: event bus closed, shutting down");
                            break;
                        }
                    }
                }

                _ = tick.tick() => {
                    self.flush_batch().await;
                }
            }
        }
    }

    // ── Event handler ─────────────────────────────────────────────────────────

    async fn handle_event(&self, event: &Event) {
        match event {
            // Individual signal — honoured only when the priority engine is NOT active.
            // When `use_priority_engine = true`, raw signals come from
            // `Event::TopSignalsBatch` (slow path) or `Event::FastSignal` (risk_engine
            // fast path), so we skip raw `Event::Signal` to avoid double-processing.
            Event::Signal(signal) if !self.config.use_priority_engine => {
                counter!("portfolio_optimizer_signals_buffered_total").increment(1);
                let mut state = self.state.write().await;
                state.signals_buffered += 1;
                state.pending_signals.push(signal.clone());
                debug!(
                    market_id = %signal.market_id,
                    ev        = signal.expected_value,
                    "portfolio_optimizer: buffered signal"
                );
            }

            // Pre-ranked batch from SignalPriorityEngine (slow path).
            // Treated identically to individual signals: pushed into the pending
            // buffer so the allocation algorithm can apply portfolio-aware sizing.
            Event::TopSignalsBatch(batch) if self.config.use_priority_engine => {
                let n = batch.signals.len() as u64;
                counter!("portfolio_optimizer_signals_buffered_total").increment(n);
                let mut state = self.state.write().await;
                state.signals_buffered += n;
                for ps in &batch.signals {
                    state.pending_signals.push(ps.signal.clone());
                }
                debug!(
                    count = n,
                    "portfolio_optimizer: buffered TopSignalsBatch"
                );
            }

            Event::Portfolio(update) => {
                // Sync our capital_available with the portfolio's free capital.
                // Portfolio.exposure is the deployed amount; free = total − exposure.
                // Normalise against config.bankroll so this stays in sync with
                // RiskState::bankroll if the operator changes the default.
                let bankroll = self.config.bankroll;
                let portfolio = &update.portfolio;
                let free_fraction = ((bankroll - portfolio.exposure) / bankroll).max(0.0);
                let mut state = self.state.write().await;
                state.capital_available = free_fraction;
                for pos in &portfolio.positions {
                    // Convert absolute size to fraction of bankroll.
                    state.positions.insert(pos.market_id.clone(), pos.size / bankroll);
                }
                debug!(
                    capital_available = state.capital_available,
                    "portfolio_optimizer: portfolio state updated"
                );
            }

            _ => {}
        }
    }

    // ── Batch flush ───────────────────────────────────────────────────────────

    /// Run the allocation algorithm over buffered signals and publish results.
    async fn flush_batch(&self) {
        // Drain pending signals under a short write-lock.
        let signals: Vec<TradeSignal> = {
            let mut state = self.state.write().await;
            std::mem::take(&mut state.pending_signals)
        };

        if signals.is_empty() {
            return;
        }

        // Read-only state for the allocator (no lock held while computing).
        let (positions, capital_available) = {
            let state = self.state.read().await;
            (state.positions.clone(), state.capital_available)
        };

        let input = AllocationInput {
            config:            &self.config,
            positions:         &positions,
            capital_available,
        };

        let output = compute_allocations(signals, &input);
        let n_emitted = output.optimized.len() as u64;

        // Update counters.
        {
            let mut state = self.state.write().await;
            state.batches_processed += 1;
            state.signals_emitted   += n_emitted;
        }

        // Publish each optimized signal.
        for opt in output.optimized {
            counter!("portfolio_optimizer_signals_emitted_total").increment(1);
            debug!(
                market_id  = %opt.signal.market_id,
                alloc      = opt.signal.position_fraction,
                raev       = opt.risk_adjusted_ev,
                "portfolio_optimizer: emitting OptimizedSignal"
            );
            if let Err(e) = self.bus.publish(Event::OptimizedSignal(opt)) {
                warn!("portfolio_optimizer: failed to publish OptimizedSignal: {e}");
            }
        }
    }
}
