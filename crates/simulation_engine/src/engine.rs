// crates/simulation_engine/src/engine.rs
//
// SimulationEngine — main struct and all simulation entry points.
//
// # Modes
//
// * LiveObserver   — default; passively tracks pipeline Execution/Portfolio
//                    events until cancelled.
// * MonteCarlo     — runs `config.monte_carlo.runs` independent internal
//                    simulations, logs summary, then falls through to
//                    LiveObserver.
// * HistoricalReplay — replays NDJSON files from `config.history_dir` as
//                    live Event::Market updates, then falls through to
//                    LiveObserver.
// * ParameterSweep — runs a two-point volatility sweep via the MC engine,
//                    logs comparison, then falls through to LiveObserver.
//
// # Internal Monte Carlo model
//
// The MC simulation does NOT publish to the Event Bus.  Instead it runs a
// self-contained mean-reversion model:
//
//   1. Generate N random-walk market paths via `MarketGenerator`.
//   2. At each tick, open a position when |p − p₀| ≥ signal_threshold.
//   3. Close the position when the deviation reverts below ½·threshold.
//   4. Track equity, drawdown, and trade PnL across all ticks.
//   5. Return one `SimulationResult` per run.

use std::path::Path;
use std::time::Duration;

use common::{Event, EventBus};
use metrics::counter;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    config::{MonteCarloConfig, ParameterSweepPoint, SimulationConfig},
    generators::MarketGenerator,
    metrics::{compute_sharpe_from_returns, SimMetricsCollector},
    replay::HistoricalReplayer,
    types::{SimulationMode, SimulationResult, SimulationTick, SweepResult},
};

// ---------------------------------------------------------------------------
// SimulationEngine
// ---------------------------------------------------------------------------

/// Drives backtesting, Monte Carlo evaluation, and live performance tracking
/// for the Prediction Market Agent pipeline.
pub struct SimulationEngine {
    /// Runtime configuration.  `pub` so tests can inspect or pre-configure.
    pub config: SimulationConfig,

    bus: EventBus,

    /// Results accumulated by completed `run_monte_carlo` / `run_parameter_sweep`
    /// calls.  Accessible via `collect_results()`.
    results: Vec<SimulationResult>,

    /// Live-mode metrics — updated by the observer loop from bus events.
    /// `pub` so integration tests can inspect live state.
    pub metrics: SimMetricsCollector,
}

impl SimulationEngine {
    /// Create and validate a new `SimulationEngine`.
    ///
    /// Returns `Err` if `config.validate()` fails (invalid field values).
    pub fn new(config: SimulationConfig, bus: EventBus) -> Result<Self, anyhow::Error> {
        if let Err(e) = config.validate() {
            return Err(anyhow::anyhow!("invalid SimulationConfig: {e}"));
        }
        let initial_capital = config.initial_capital;
        Ok(Self {
            config,
            bus,
            results: Vec::new(),
            metrics: SimMetricsCollector::with_initial_capital(initial_capital),
        })
    }

    // ── Public simulation API ─────────────────────────────────────────────────

    /// Run the internal Monte Carlo simulation.
    ///
    /// Generates `config.monte_carlo.runs` independent random market paths
    /// using `MarketGenerator` and a simple mean-reversion signal model.
    ///
    /// **Does not publish to the Event Bus.**  For end-to-end pipeline
    /// backtesting use `run_historical_replay()` instead.
    ///
    /// Results are also appended to `self.results` and accessible via
    /// `collect_results()`.
    pub fn run_monte_carlo(&mut self) -> Vec<SimulationResult> {
        let n_runs = self.config.monte_carlo.runs;
        info!(
            n_runs,
            max_ticks  = self.config.max_ticks,
            volatility = self.config.monte_carlo.volatility,
            drift      = self.config.monte_carlo.drift,
            "simulation_engine: Monte Carlo starting"
        );

        let mut results = Vec::with_capacity(n_runs);
        for run_id in 0..n_runs {
            // Give each run a unique seed derived from the base seed so paths
            // are independent yet still reproducible when a seed is set.
            let seed = self.config.monte_carlo.seed.map(|s| s.wrapping_add(run_id as u64));
            let mc_cfg = MonteCarloConfig { seed, ..self.config.monte_carlo.clone() };
            let result = self.simulate_single_path(run_id, mc_cfg);
            debug!(
                run_id,
                total_return = result.total_return,
                sharpe       = result.sharpe_ratio,
                max_drawdown = result.max_drawdown,
                trades       = result.trades_executed,
                "simulation_engine: MC run complete"
            );
            counter!("simulation_engine_mc_runs_total").increment(1);
            results.push(result);
        }

        let avg_return = results.iter().map(|r| r.total_return).sum::<f64>() / n_runs as f64;
        info!(
            n_runs,
            avg_return,
            "simulation_engine: Monte Carlo complete"
        );

        self.results.extend(results.clone());
        results
    }

    /// Replay historical NDJSON data from `config.history_dir`, publishing
    /// each tick's `MarketUpdate`s as `Event::Market` on the Event Bus.
    ///
    /// The full downstream pipeline (graph → bayesian → signal → optimizer →
    /// risk → execution → portfolio → analytics) processes these events as if
    /// they were live.  Subscribe to `Event::Portfolio` on the bus to observe
    /// performance.
    ///
    /// Returns the number of ticks published.
    pub async fn run_historical_replay(&mut self) -> usize {
        // File loading is synchronous and potentially slow — move it off the
        // async executor to avoid blocking other tasks.
        let dir_str = self.config.history_dir.clone();
        let mut replayer = tokio::task::spawn_blocking(move || {
            HistoricalReplayer::from_dir(Path::new(&dir_str))
        })
        .await
        .expect("replay loader panicked");

        let dir = Path::new(&self.config.history_dir);
        let total = replayer.remaining();
        info!(
            total_ticks = total,
            directory   = %dir.display(),
            "simulation_engine: historical replay starting"
        );

        if replayer.is_empty() {
            warn!(
                "simulation_engine: no historical data found in {}",
                dir.display()
            );
            return 0;
        }

        let interval = Duration::from_millis(self.config.tick_interval_ms);
        let mut published = 0usize;

        while let Some(tick) = replayer.next_tick() {
            for update in tick.market_updates {
                // Ignore send errors (no subscribers yet during startup).
                let _ = self.bus.publish(Event::Market(update));
            }
            published += 1;
            counter!("simulation_engine_replay_ticks_total").increment(1);

            if !interval.is_zero() {
                tokio::time::sleep(interval).await;
            }
        }

        info!(
            published,
            "simulation_engine: historical replay complete"
        );
        published
    }

    /// Run a parameter sweep over a set of `ParameterSweepPoint`s.
    ///
    /// For each point the internal Monte Carlo is run and the results are
    /// aggregated into a `SweepResult`.  The original `monte_carlo` config is
    /// restored after the sweep.
    pub fn run_parameter_sweep(
        &mut self,
        points: Vec<ParameterSweepPoint>,
    ) -> Vec<SweepResult> {
        info!(
            n_points = points.len(),
            "simulation_engine: parameter sweep starting"
        );
        let orig_mc = self.config.monte_carlo.clone();
        let mut sweep_results = Vec::with_capacity(points.len());

        for point in &points {
            self.config.monte_carlo = point.mc_config.clone();
            let results = self.run_monte_carlo();
            sweep_results.push(SweepResult::from_results(
                point.label.clone(),
                point.mc_config.volatility,
                point.mc_config.drift,
                results,
            ));
        }

        // Restore original MC config.
        self.config.monte_carlo = orig_mc;
        sweep_results
    }

    /// Generate a single `SimulationTick` using the configured market specs.
    ///
    /// This is a one-shot helper — it builds a temporary `MarketGenerator`
    /// each time.  For streaming tick generation use `MarketGenerator` directly.
    pub fn generate_market_tick(&mut self) -> SimulationTick {
        let mut gen = MarketGenerator::new(
            self.config.markets.clone(),
            self.config.monte_carlo.clone(),
        );
        gen.next_tick()
    }

    /// All `SimulationResult`s from completed `run_monte_carlo` calls.
    pub fn collect_results(&self) -> &[SimulationResult] {
        &self.results
    }

    // ── Main async entry point ────────────────────────────────────────────────

    /// Main async loop — spawned by the orchestrator.
    ///
    /// 1. Executes the one-time action implied by `config.mode`
    ///    (MonteCarlo / HistoricalReplay / ParameterSweep).
    /// 2. Always ends in `LiveObserver` mode, collecting Execution and
    ///    Portfolio events from the bus until `cancel` is triggered.
    ///
    /// CPU-bound modes (MonteCarlo, ParameterSweep) are off-loaded to a
    /// `spawn_blocking` thread so the Tokio executor is not starved.
    pub async fn run(self, cancel: CancellationToken) {
        let mode = self.config.mode;
        info!("simulation_engine: started (mode={:?})", mode);

        // ── One-time startup action based on mode ─────────────────────────
        let this = match mode {
            SimulationMode::MonteCarlo => {
                // CPU-bound: move to a blocking thread.
                tokio::task::spawn_blocking(move || {
                    let mut eng = self;
                    let results = eng.run_monte_carlo();
                    log_mc_summary(&results);
                    eng
                })
                .await
                .expect("MC simulation panicked")
            }
            SimulationMode::HistoricalReplay => {
                let mut eng = self;
                eng.run_historical_replay().await;
                eng
            }
            SimulationMode::ParameterSweep => {
                // CPU-bound: move to a blocking thread.
                tokio::task::spawn_blocking(move || {
                    let mut eng = self;
                    let points = default_sweep_points();
                    let sweep = eng.run_parameter_sweep(points);
                    log_sweep_summary(&sweep);
                    eng
                })
                .await
                .expect("parameter sweep panicked")
            }
            SimulationMode::LiveObserver => self,
        };

        // ── Passive observer loop ─────────────────────────────────────────
        this.run_observer_loop(cancel).await;
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    /// Subscribe to the bus and feed Execution / Portfolio events into the
    /// metrics collector until `cancel` fires or the bus closes.
    async fn run_observer_loop(mut self, cancel: CancellationToken) {
        let mut rx = self.bus.subscribe();
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    info!("simulation_engine: shutdown — final metrics: {} executions, {} portfolio snapshots",
                        self.metrics.total_executions, self.metrics.snapshot_count());
                    break;
                }
                ev = rx.recv() => match ev {
                    Ok(arc_ev) => match arc_ev.as_ref() {
                        Event::Execution(r) => {
                            self.metrics.on_execution(r);
                            counter!("simulation_engine_executions_observed_total").increment(1);
                        }
                        Event::Portfolio(u) => {
                            self.metrics.on_portfolio(u);
                            counter!("simulation_engine_portfolio_snapshots_total").increment(1);
                        }
                        _ => {}
                    },
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("simulation_engine: lagged by {n} events");
                        counter!("simulation_engine_lagged_total").increment(1);
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        info!("simulation_engine: bus closed, shutting down");
                        break;
                    }
                }
            }
        }
    }

    /// Simulate a single Monte Carlo path using the internal mean-reversion
    /// signal model.
    ///
    /// # Signal model
    ///
    /// * **Entry**: open when `|p(t) − p₀| ≥ signal_threshold`.
    ///   Direction: BUY when price is below fair value, SELL when above.
    /// * **Exit**: close when the deviation falls below `½·signal_threshold`
    ///   (mean reversion) or the position changes sign.
    /// * **PnL**: `(exit_prob − entry_prob) × pos_size` for BUY,
    ///            `(entry_prob − exit_prob) × pos_size` for SELL.
    fn simulate_single_path(&self, run_id: usize, mc_cfg: MonteCarloConfig) -> SimulationResult {
        let mut gen      = MarketGenerator::new(self.config.markets.clone(), mc_cfg);
        let initial_cap  = self.config.initial_capital;
        let mut equity   = initial_cap;
        let mut peak_eq  = initial_cap;
        let mut max_dd   = 0.0_f64;
        let threshold    = self.config.signal_threshold;
        let pos_fraction = self.config.position_fraction;

        let n_markets = self.config.markets.len();
        // (entry_prob, direction, entry_pos_size)  —  None means no open position.
        // The position size is captured at entry so PnL is not distorted by
        // equity changes that occur while the position is open.
        let mut positions: Vec<Option<(f64, TradeDir, f64)>> = vec![None; n_markets];

        let mut equity_series: Vec<f64> = Vec::with_capacity(self.config.max_ticks);
        let mut trade_pnls:    Vec<f64> = Vec::new();

        for _tick in 0..self.config.max_ticks {
            let sim_tick = gen.next_tick();

            for (i, update) in sim_tick.market_updates.iter().enumerate() {
                let cur_prob   = update.market.probability;
                let init_prob  = self.config.markets[i].initial_prob;
                let deviation  = cur_prob - init_prob;
                let pos_size   = pos_fraction * equity;

                match positions[i] {
                    None => {
                        // Entry check.
                        if deviation.abs() >= threshold {
                            let dir = if deviation > 0.0 { TradeDir::Sell } else { TradeDir::Buy };
                            // Capture pos_size at entry so PnL is not distorted
                            // by equity changes while the position is open.
                            positions[i] = Some((cur_prob, dir, pos_size));
                        }
                    }
                    Some((entry_prob, dir, entry_size)) => {
                        // Exit when deviation has reverted sufficiently.
                        let still_open = match dir {
                            TradeDir::Buy  => deviation < -threshold * 0.5,
                            TradeDir::Sell => deviation >  threshold * 0.5,
                        };
                        if !still_open {
                            let pnl = match dir {
                                TradeDir::Buy  => (cur_prob   - entry_prob) * entry_size,
                                TradeDir::Sell => (entry_prob - cur_prob)   * entry_size,
                            };
                            equity += pnl;
                            if !equity.is_finite() {
                                warn!(
                                    run_id,
                                    "simulation_engine: equity became non-finite, \
                                     terminating path early"
                                );
                                break;
                            }
                            trade_pnls.push(pnl);
                            positions[i] = None;
                        }
                    }
                }
            }

            // Equity curve.
            equity_series.push(equity);
            if equity > peak_eq {
                peak_eq = equity;
            } else {
                let dd = (peak_eq - equity) / peak_eq.max(1.0);
                max_dd = max_dd.max(dd);
            }
        }

        // ── Aggregate stats ───────────────────────────────────────────────
        let final_equity  = equity_series.last().copied().unwrap_or(initial_cap);
        let total_return  = (final_equity - initial_cap) / initial_cap;
        let returns: Vec<f64> = equity_series.windows(2).map(|w| w[1] - w[0]).collect();
        let sharpe        = compute_sharpe_from_returns(&returns);
        let wins          = trade_pnls.iter().filter(|&&p| p > 0.0).count();
        let win_rate      = if trade_pnls.is_empty() { 0.0 } else { wins as f64 / trade_pnls.len() as f64 };

        SimulationResult {
            run_id,
            total_return,
            sharpe_ratio:    sharpe,
            max_drawdown:    max_dd,
            win_rate,
            trades_executed: trade_pnls.len(),
            final_equity,
        }
    }
}

// ---------------------------------------------------------------------------
// Private types and helpers
// ---------------------------------------------------------------------------

/// Internal trade direction used by the MC signal model.
#[derive(Clone, Copy, Debug)]
enum TradeDir { Buy, Sell }

/// Log a Monte Carlo batch summary at INFO level.
fn log_mc_summary(results: &[SimulationResult]) {
    if results.is_empty() {
        return;
    }
    let n = results.len() as f64;
    let avg_ret = results.iter().map(|r| r.total_return).sum::<f64>()  / n;
    let avg_sh  = results.iter().map(|r| r.sharpe_ratio).sum::<f64>()  / n;
    let worst   = results.iter().map(|r| r.max_drawdown).fold(0.0_f64, f64::max);
    info!(
        runs         = results.len(),
        avg_return   = avg_ret,
        avg_sharpe   = avg_sh,
        worst_drawdown = worst,
        "simulation_engine: Monte Carlo summary"
    );
}

/// Log a parameter sweep summary at INFO level.
fn log_sweep_summary(sweep: &[SweepResult]) {
    for sr in sweep {
        info!(
            label        = %sr.label,
            volatility   = sr.mc_config_volatility,
            avg_return   = sr.avg_return,
            avg_sharpe   = sr.avg_sharpe,
            avg_drawdown = sr.avg_max_drawdown,
            "simulation_engine: sweep result"
        );
    }
}

/// Default two-point volatility sweep used in `ParameterSweep` mode.
fn default_sweep_points() -> Vec<ParameterSweepPoint> {
    vec![
        ParameterSweepPoint {
            label:     "low-vol".to_string(),
            mc_config: MonteCarloConfig {
                volatility: 0.01,
                drift:      0.0,
                runs:       20,
                ..MonteCarloConfig::default()
            },
        },
        ParameterSweepPoint {
            label:     "high-vol".to_string(),
            mc_config: MonteCarloConfig {
                volatility: 0.05,
                drift:      0.0,
                runs:       20,
                ..MonteCarloConfig::default()
            },
        },
    ]
}
