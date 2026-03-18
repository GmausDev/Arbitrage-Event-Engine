// crates/strategy_research/src/evaluator.rs
//
// StrategyEvaluator — computes performance metrics from a BacktestResult.
//
// Annualisation convention: √252 (treating each backtest tick as one trading
// day), matching the simulation_engine's `compute_sharpe_from_returns`.

use crate::types::{BacktestResult, StrategyMetrics};
use uuid::Uuid;

/// Annualisation factor for Sharpe ratio (√252 — daily-period convention).
const ANN_FACTOR: f64 = 252.0_f64;

/// Compute performance metrics from a completed backtest.
pub fn evaluate(result: &BacktestResult) -> StrategyMetrics {
    let trade_count = result.trades.len();

    // Guard: no trades or no equity history → worst-case metrics.
    if trade_count == 0 || result.equity_curve.is_empty() {
        return zero_metrics(result.strategy_id);
    }

    let final_equity = *result.equity_curve.last().unwrap();
    let total_return = (final_equity - result.initial_capital) / result.initial_capital.max(1e-10);

    let wins = result.trades.iter().filter(|t| t.pnl_abs > 0.0).count();
    let win_rate = wins as f64 / trade_count as f64;
    let avg_trade_pnl = result.trades.iter().map(|t| t.pnl_abs).sum::<f64>()
        / trade_count as f64;

    let tick_returns = equity_tick_returns(&result.equity_curve);
    let volatility = stddev(&tick_returns);
    let sharpe_ratio = sharpe(&tick_returns);
    let max_drawdown = max_drawdown(&result.equity_curve);

    StrategyMetrics {
        strategy_id: result.strategy_id,
        sharpe_ratio,
        max_drawdown,
        win_rate,
        trade_count,
        total_return,
        volatility,
        avg_trade_pnl,
    }
}

// ---------------------------------------------------------------------------
// Core metric functions (also pub for unit testing)
// ---------------------------------------------------------------------------

/// Annualised Sharpe ratio from a series of per-tick equity returns.
///
/// Returns `f64::NEG_INFINITY` when fewer than 2 data points.
/// Returns `f64::INFINITY` when mean > 0 and std ≈ 0 (monotone profit).
pub fn sharpe(returns: &[f64]) -> f64 {
    if returns.len() < 2 {
        return f64::NEG_INFINITY;
    }
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let std = stddev(returns);
    if std < 1e-12 {
        return if mean > 0.0 {
            f64::INFINITY
        } else {
            f64::NEG_INFINITY
        };
    }
    (mean / std) * ANN_FACTOR.sqrt()
}

/// Sample standard deviation.
pub fn stddev(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>()
        / (values.len() - 1) as f64;
    var.sqrt()
}

/// Peak-to-trough maximum drawdown as a fraction of peak equity.
pub fn max_drawdown(equity_curve: &[f64]) -> f64 {
    if equity_curve.is_empty() {
        return 0.0;
    }
    let mut peak = equity_curve[0];
    let mut max_dd = 0.0_f64;
    for &eq in equity_curve {
        if eq > peak {
            peak = eq;
        }
        let dd = if peak > 0.0 { (peak - eq) / peak } else { 0.0 };
        if dd > max_dd {
            max_dd = dd;
        }
    }
    max_dd
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Compute per-tick returns: `(equity[i] − equity[i−1]) / equity[i−1]`.
fn equity_tick_returns(equity_curve: &[f64]) -> Vec<f64> {
    equity_curve
        .windows(2)
        .map(|w| {
            let prev = w[0].max(1e-10);
            (w[1] - w[0]) / prev
        })
        .collect()
}

fn zero_metrics(strategy_id: Uuid) -> StrategyMetrics {
    StrategyMetrics {
        strategy_id,
        sharpe_ratio: f64::NEG_INFINITY,
        max_drawdown: 1.0,
        win_rate: 0.0,
        trade_count: 0,
        total_return: 0.0,
        volatility: 0.0,
        avg_trade_pnl: 0.0,
    }
}
