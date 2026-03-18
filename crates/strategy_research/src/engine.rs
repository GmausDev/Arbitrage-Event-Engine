// crates/strategy_research/src/engine.rs
//
// StrategyResearchEngine — async event loop that drives the automated
// hypothesis-generate → backtest → evaluate → promote pipeline.
//
// Lifecycle
// ─────────
//   1. On startup, arm a research timer (interval = research_interval_secs).
//   2. On each timer tick, run one research cycle (async):
//        a. Generate `batch_size` hypotheses.
//        b. Compile each to a GeneratedStrategy.
//        c. Backtest all strategies in parallel (spawn_blocking + Semaphore).
//        d. Evaluate metrics; strategies that pass thresholds → registry.
//        e. Emit Event::StrategyDiscovered for each newly promoted strategy.
//   3. Also consume Event::Portfolio (for telemetry; future: live metric seeding).
//   4. Shut down gracefully on CancellationToken.

use std::sync::Arc;

use common::{
    events::StrategyDiscoveredEvent,
    Event, EventBus,
};
use metrics::counter;
use tokio::{
    sync::{broadcast, RwLock, Semaphore},
    time::{interval, Duration},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    backtest::run_backtest,
    compiler::StrategyCompiler,
    config::ResearchConfig,
    evaluator::evaluate,
    hypothesis::HypothesisGenerator,
    registry::StrategyRegistry,
    templates::all_templates,
    types::Hypothesis,
};

// ---------------------------------------------------------------------------
// Engine struct
// ---------------------------------------------------------------------------

pub struct StrategyResearchEngine {
    pub config: ResearchConfig,
    /// Shared registry — `pub` so callers can inspect results in tests.
    pub registry: Arc<RwLock<StrategyRegistry>>,
    bus: EventBus,
    rx: Option<broadcast::Receiver<Arc<Event>>>,
}

impl StrategyResearchEngine {
    pub fn new(config: ResearchConfig, bus: EventBus) -> Self {
        config.validate().expect("ResearchConfig is invalid");
        let rx = bus.subscribe();
        let registry = Arc::new(RwLock::new(StrategyRegistry::new(&config)));
        Self {
            config,
            registry,
            bus,
            rx: Some(rx),
        }
    }

    // ── Public entry point ────────────────────────────────────────────────

    pub async fn run(mut self, cancel: CancellationToken) {
        let mut rx = self.rx.take().expect("rx already consumed");
        let mut timer = interval(Duration::from_secs(self.config.research_interval_secs));
        // First tick fires immediately → first cycle starts at boot.

        info!(
            batch_size = self.config.batch_size,
            interval_secs = self.config.research_interval_secs,
            min_sharpe = self.config.min_sharpe,
            "strategy_research_engine: starting"
        );

        let mut cycle: u64 = 0;

        loop {
            tokio::select! {
                biased;

                _ = cancel.cancelled() => {
                    info!("strategy_research_engine: shutdown signal received");
                    break;
                }

                _ = timer.tick() => {
                    cycle += 1;
                    info!(cycle, "strategy_research_engine: research cycle starting");
                    self.run_research_cycle(cycle).await;
                }

                event = rx.recv() => {
                    match event {
                        Ok(ev) => self.handle_event(ev).await,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(dropped = n, "strategy_research_engine: event bus lagged");
                            counter!("strategy_research_events_lagged_total").increment(n);
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("strategy_research_engine: event bus closed, stopping");
                            break;
                        }
                    }
                }
            }
        }
    }

    // ── Event handler (telemetry only for now) ────────────────────────────

    async fn handle_event(&self, event: Arc<Event>) {
        match event.as_ref() {
            Event::Portfolio(_) => {
                counter!("strategy_research_portfolio_events_total").increment(1);
                // Future: seed live market snapshots for hypothesis refinement.
            }
            _ => {}
        }
    }

    // ── Research cycle ────────────────────────────────────────────────────

    async fn run_research_cycle(&self, cycle: u64) {
        // ── 1. Generate hypotheses ────────────────────────────────────────
        let seed = self.config.seed.map(|s| s.wrapping_add(cycle));
        let mut gen = HypothesisGenerator::new(seed, all_templates());
        let hypotheses = gen.generate(self.config.batch_size);
        debug!(count = hypotheses.len(), "strategy_research_engine: hypotheses generated");

        // ── 2. Compile ────────────────────────────────────────────────────
        let strategies: Vec<_> = hypotheses.into_iter().map(StrategyCompiler::compile).collect();
        let total = strategies.len();

        // ── 3. Backtest in parallel ───────────────────────────────────────
        let semaphore = Arc::new(Semaphore::new(self.config.max_parallel_backtests));
        let cfg = Arc::new(self.config.clone());
        let base_seed = cycle.wrapping_mul(0xDEAD_BEEF_CAFE_u64);

        let handles: Vec<_> = strategies
            .into_iter()
            .enumerate()
            .map(|(idx, strategy)| {
                let sem = semaphore.clone();
                let cfg = cfg.clone();
                let seed = base_seed.wrapping_add(idx as u64);

                tokio::spawn(async move {
                    // Acquire a permit before entering the blocking pool.
                    let permit = sem
                        .acquire_owned()
                        .await
                        .expect("research semaphore closed");

                    tokio::task::spawn_blocking(move || {
                        let _p = permit; // released when blocking task finishes
                        let result = run_backtest(&strategy, &cfg, seed);
                        let metrics = evaluate(&result);
                        let hypothesis: Hypothesis = strategy.hypothesis;
                        (hypothesis, metrics)
                    })
                    .await
                    .ok() // None on panic
                })
            })
            .collect();

        // ── 4. Collect, evaluate, promote ─────────────────────────────────
        let mut promoted = 0_usize;
        let mut evaluated = 0_usize;

        for handle in handles {
            let inner = match handle.await {
                Ok(Some(pair)) => pair,
                Ok(None) => continue, // blocking task panicked
                Err(_) => continue,   // spawn error
            };

            let (hypothesis, metrics) = inner;
            evaluated += 1;
            counter!("strategy_research_backtests_total").increment(1);

            if !StrategyRegistry::meets_promotion_criteria(&metrics, &cfg) {
                continue;
            }

            debug!(
                sharpe = metrics.sharpe_ratio,
                drawdown = metrics.max_drawdown,
                trades = metrics.trade_count,
                template = hypothesis.template_name,
                "strategy_research_engine: strategy meets promotion criteria"
            );

            // Drop the write lock before serialising and publishing to avoid
            // holding it across non-trivial work (project lock discipline).
            let discovered = {
                let mut reg = self.registry.write().await;
                reg.try_insert(hypothesis, metrics)
            };

            if let Some(discovered) = discovered {
                promoted += 1;
                counter!("strategy_research_promoted_total").increment(1);

                // Serialise hypothesis for the event payload.
                let rule_json = serde_json::to_string(&discovered.hypothesis)
                    .unwrap_or_else(|_| "{}".into());

                let event = Event::StrategyDiscovered(StrategyDiscoveredEvent {
                    strategy_id: discovered.id.clone(),
                    rule_definition_json: rule_json,
                    sharpe: discovered.metrics.sharpe_ratio,
                    max_drawdown: discovered.metrics.max_drawdown,
                    win_rate: discovered.metrics.win_rate,
                    trade_count: discovered.metrics.trade_count,
                    promoted_at: discovered.promoted_at,
                });

                if let Err(e) = self.bus.publish(event) {
                    warn!(
                        err = %e,
                        id = discovered.id,
                        "strategy_research_engine: failed to publish StrategyDiscovered"
                    );
                }
            }
        }

        let registry_size = self.registry.read().await.len();

        info!(
            cycle,
            evaluated,
            total,
            promoted,
            registry_size,
            "strategy_research_engine: cycle complete"
        );

        counter!("strategy_research_cycles_total").increment(1);
    }
}
