// crates/simulation_engine/src/metrics.rs
//
// Live-mode performance metrics collector.
//
// `SimMetricsCollector` subscribes (indirectly, via the engine's event loop)
// to `Event::Execution` and `Event::Portfolio` and accumulates the data
// needed to compute a `SimulationResult` at any point in time.

use common::{ExecutionResult, PortfolioUpdate};

use crate::types::SimulationResult;

// ---------------------------------------------------------------------------
// SimMetricsCollector
// ---------------------------------------------------------------------------

/// Accumulates live pipeline events to compute `SimulationResult` on demand.
///
/// This is used by `SimulationEngine` in `LiveObserver` mode to track the
/// real pipeline's performance without interrupting it.
#[derive(Debug, Default)]
pub struct SimMetricsCollector {
    /// Initial capital offset so equity = initial_capital + portfolio.pnl
    /// (keeps equity positive and drawdown meaningful even when PnL is negative).
    initial_capital: f64,
    /// Total equity observed at each `Event::Portfolio` snapshot, in order.
    equity_series: Vec<f64>,
    /// Per-trade PnL proxy: negative slippage × quantity for each filled execution.
    trade_pnls:    Vec<f64>,
    /// Running high-water mark for drawdown calculation.
    peak_equity:   f64,
    /// Maximum observed peak-to-trough drawdown fraction.
    max_drawdown:  f64,
    /// Total executions processed (filled and unfilled).
    pub total_executions: usize,
}

impl SimMetricsCollector {
    /// Create a new collector with no initial capital offset.
    ///
    /// For live-mode use, prefer `with_initial_capital` so that drawdown is
    /// computed against total equity rather than raw PnL.
    pub fn new() -> Self {
        Self::with_initial_capital(0.0)
    }

    /// Create a new collector that offsets portfolio PnL by `initial_capital`
    /// so that the internal equity series reflects total portfolio value.
    pub fn with_initial_capital(initial_capital: f64) -> Self {
        Self {
            initial_capital,
            equity_series:   Vec::new(),
            trade_pnls:      Vec::new(),
            peak_equity:     f64::NEG_INFINITY,
            max_drawdown:    0.0,
            total_executions: 0,
        }
    }

    /// Record a simulated execution result.
    ///
    /// Only filled executions contribute to trade PnL tracking.  The PnL
    /// proxy used here is `−slippage × executed_quantity`, which captures
    /// fill quality rather than directional PnL (the portfolio engine owns
    /// the authoritative PnL).
    pub fn on_execution(&mut self, result: &ExecutionResult) {
        self.total_executions += 1;
        if result.filled {
            let pnl_proxy = -result.slippage * result.executed_quantity;
            self.trade_pnls.push(pnl_proxy);
        }
    }

    /// Record a portfolio snapshot.
    ///
    /// `portfolio.pnl` is the total (realised + unrealised) PnL in USD as
    /// reported by the portfolio engine.  The stored equity value is
    /// `initial_capital + pnl`, keeping it positive so drawdown is meaningful
    /// even when PnL is negative.
    pub fn on_portfolio(&mut self, update: &PortfolioUpdate) {
        let equity = self.initial_capital + update.portfolio.pnl;
        self.equity_series.push(equity);

        // Update high-water mark and running max drawdown.
        if equity > self.peak_equity {
            self.peak_equity = equity;
        } else if self.peak_equity > f64::NEG_INFINITY {
            let dd = (self.peak_equity - equity) / self.peak_equity.max(f64::EPSILON);
            if dd > self.max_drawdown {
                self.max_drawdown = dd;
            }
        }
    }

    /// Compute a `SimulationResult` from all accumulated data.
    ///
    /// `initial_capital` is used as the denominator for `total_return`
    /// (`(final_equity − initial_capital) / initial_capital`).
    pub fn compute_result(&self, run_id: usize, initial_capital: f64) -> SimulationResult {
        let final_equity = self.equity_series.last().copied().unwrap_or(0.0);
        let total_return = if initial_capital > 0.0 {
            (final_equity - initial_capital) / initial_capital
        } else {
            0.0
        };

        SimulationResult {
            run_id,
            total_return,
            sharpe_ratio:    self.compute_sharpe(),
            max_drawdown:    self.max_drawdown,
            win_rate:        self.compute_win_rate(),
            trades_executed: self.trade_pnls.len(),
            final_equity,
        }
    }

    /// Reset all accumulated data.  Call between simulation runs.
    /// The `initial_capital` offset is preserved across resets.
    pub fn reset(&mut self) {
        self.equity_series.clear();
        self.trade_pnls.clear();
        self.peak_equity  = f64::NEG_INFINITY;
        self.max_drawdown = 0.0;
        self.total_executions = 0;
    }

    /// Number of portfolio snapshots observed.
    pub fn snapshot_count(&self) -> usize {
        self.equity_series.len()
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    fn compute_sharpe(&self) -> f64 {
        let returns = self.tick_returns();
        compute_sharpe_from_returns(&returns)
    }

    fn compute_win_rate(&self) -> f64 {
        if self.trade_pnls.is_empty() {
            return 0.0;
        }
        let wins = self.trade_pnls.iter().filter(|&&p| p > 0.0).count();
        wins as f64 / self.trade_pnls.len() as f64
    }

    /// Tick-to-tick PnL differences.
    fn tick_returns(&self) -> Vec<f64> {
        self.equity_series
            .windows(2)
            .map(|w| w[1] - w[0])
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Shared Sharpe utility (also used by engine.rs)
// ---------------------------------------------------------------------------

/// Compute an annualised Sharpe ratio from a slice of period returns.
///
/// Uses a √252 annualisation factor (daily periods).  Returns `0.0` when
/// fewer than two data points are available or when σ ≈ 0.
pub(crate) fn compute_sharpe_from_returns(returns: &[f64]) -> f64 {
    if returns.len() < 2 {
        return 0.0;
    }
    let n    = returns.len() as f64;
    let mean = returns.iter().sum::<f64>() / n;
    let var  = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0);
    let std  = var.sqrt();
    if std < f64::EPSILON {
        return 0.0;
    }
    const ANN: f64 = 252.0;
    mean / std * ANN.sqrt()
}
