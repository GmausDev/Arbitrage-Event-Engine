// tests/cost_model_backtest/src/main.rs
//
// Before/After Net Edge Model Backtest
//
// Runs synthetic Monte Carlo paths with two independent trading strategies on
// IDENTICAL market price paths:
//
//   Before — accepts any signal with gross_edge >= THRESHOLD (no cost filter).
//            Represents the pre-cost-model system that treated edge ≥ min_ev as
//            sufficient — ignoring fees, spread, slippage and latency decay.
//
//   After  — same entry threshold, but additionally gates on:
//              net_edge  =  gross_edge − (fees + spread + slippage + decay) > 0
//              expected_profit  =  net_edge × position_usd  ≥  min_profit_usd
//            Represents the system after integrating cost_model into risk_engine.
//
// Market model (matches strategy_research/src/backtest.rs):
//   • 5 correlated random-walk markets, common-factor Brownian motion.
//   • Mean reversion toward initial probabilities.
//   • Bayesian EMA posterior: posterior = 0.80·prev + 0.20·market.
//   • Signal: |posterior − market| ≥ THRESHOLD.
//   • Fixed hold_ticks (20) before position is force-closed.
//   • Independent position tracker per strategy, so the two strategies
//     experience different occupancy but identical market prices.

use cost_model::{
    compute_cost_estimate, compute_expected_profit, compute_net_edge, CostModelConfig,
};
use rand::prelude::*;
use rand_distr::{Distribution, StandardNormal};
use rayon::prelude::*;

// ── Simulation parameters ──────────────────────────────────────────────────────

/// Number of independent Monte Carlo paths.
const NUM_PATHS: usize = 2_000;

/// Ticks simulated per path.
const HORIZON: usize = 1_000;

/// Number of synthetic markets.
const N: usize = 5;

/// Starting bankroll in USD.
const INITIAL_CAPITAL: f64 = 10_000.0;

/// Per-tick market volatility in probability units (matches strategy_research default).
const VOLATILITY: f64 = 0.015;

/// Cross-market price correlation (matches strategy_research default).
const CORRELATION: f64 = 0.30;

/// Drift (risk-neutral: zero).
const DRIFT: f64 = 0.0;

/// Mean-reversion speed toward initial probabilities.
const MEAN_REVERSION: f64 = 0.05;

/// Position size as fraction of current equity (Kelly-inspired, matches default config).
const POS_FRAC: f64 = 0.05;

/// Fixed position holding period in ticks.
const HOLD_TICKS: usize = 20;

/// Minimum gross_edge to generate a signal.  Intentionally low so the cost model
/// does the filtering rather than the signal threshold.
const SIGNAL_THRESHOLD: f64 = 0.01;

/// RNG seed for full reproducibility.
const SEED: u64 = 42;

/// Initial probability for each of the N markets.
const INIT_PROBS: [f64; N] = [0.30, 0.40, 0.50, 0.60, 0.70];

// ── Per-path result ────────────────────────────────────────────────────────────

struct PathResult {
    // Returns: (final_equity − initial) / initial
    before_return: f64,
    after_return:  f64,
    // Per-path maximum peak-to-trough drawdown fraction ∈ [0, 1].
    before_max_dd: f64,
    after_max_dd:  f64,
    // Trade counts.
    before_trades: u64,
    after_trades:  u64,
    /// Signals the After strategy COULD have taken (position slot free, ge ≥ threshold)
    /// but which were blocked by the cost model gate.
    cost_rejected: u64,
    // Cumulative edge sums (for per-trade averages).
    before_gross_sum: f64,
    before_net_sum:   f64,
    after_gross_sum:  f64,
    after_net_sum:    f64,
}

// ── Core simulation ────────────────────────────────────────────────────────────

/// Open position: (entry_prob, open_tick, is_buy, equity_at_entry_usd).
type Pos = Option<(f64, usize, bool, f64)>;

fn run_path(path_id: usize, cost_cfg: &CostModelConfig) -> PathResult {
    let mut rng = StdRng::seed_from_u64(SEED.wrapping_add(path_id as u64));

    let mut probs      = INIT_PROBS;
    let mut posteriors = INIT_PROBS;

    // Independent position trackers — same market prices, independent decisions.
    let mut bp: [Pos; N] = [None; N]; // Before positions
    let mut ap: [Pos; N] = [None; N]; // After positions

    let mut beq: f64 = INITIAL_CAPITAL;
    let mut aeq: f64 = INITIAL_CAPITAL;
    let mut bpeak = INITIAL_CAPITAL;
    let mut apeak = INITIAL_CAPITAL;
    let mut bdd = 0.0_f64;
    let mut add = 0.0_f64;

    let mut btrades  = 0u64;
    let mut atrades  = 0u64;
    let mut rejected = 0u64;

    let mut bgross = 0.0_f64;
    let mut bnet   = 0.0_f64;
    let mut agross = 0.0_f64;
    let mut anet   = 0.0_f64;

    for tick in 0..HORIZON {
        // ── Step 1: correlated market evolution ────────────────────────────────
        let cz: f64 = StandardNormal.sample(&mut rng);
        for i in 0..N {
            let iz: f64 = StandardNormal.sample(&mut rng);
            let z = CORRELATION.sqrt() * cz + (1.0 - CORRELATION).sqrt() * iz;
            let mr = MEAN_REVERSION * (INIT_PROBS[i] - probs[i]);
            probs[i] = (probs[i] + VOLATILITY * z + DRIFT + mr).clamp(0.01, 0.99);
            posteriors[i] = 0.80 * posteriors[i] + 0.20 * probs[i];
        }

        // ── Step 2: close expired positions ────────────────────────────────────
        for i in 0..N {
            if let Some((ep, ot, buy, eeq)) = bp[i] {
                if tick >= ot + HOLD_TICKS {
                    let pnl = if buy { probs[i] - ep } else { ep - probs[i] };
                    beq += pnl * POS_FRAC * eeq;
                    if beq > bpeak { bpeak = beq; }
                    bp[i] = None;
                }
            }
            if let Some((ep, ot, buy, eeq)) = ap[i] {
                if tick >= ot + HOLD_TICKS {
                    let pnl = if buy { probs[i] - ep } else { ep - probs[i] };
                    aeq += pnl * POS_FRAC * eeq;
                    if aeq > apeak { apeak = aeq; }
                    ap[i] = None;
                }
            }
        }

        // ── Step 3: update running drawdown ───────────────────────────────────
        if bpeak > 0.0 { bdd = bdd.max((bpeak - beq) / bpeak); }
        if apeak > 0.0 { add = add.max((apeak - aeq) / apeak); }

        // ── Step 4: evaluate signals for both strategies ───────────────────────
        for i in 0..N {
            let ge = (posteriors[i] - probs[i]).abs();
            if ge < SIGNAL_THRESHOLD { continue; }

            let is_buy = posteriors[i] > probs[i];

            // Cost estimate shared between both strategies for this signal.
            let cost  = compute_cost_estimate(ge, POS_FRAC, VOLATILITY,
                                              cost_cfg.expected_latency_ms, cost_cfg);
            let ne    = compute_net_edge(ge, &cost);

            // ── Before: accept any signal above threshold ──────────────────────
            if bp[i].is_none() {
                bp[i] = Some((probs[i], tick, is_buy, beq));
                btrades += 1;
                bgross  += ge;
                bnet    += ne; // track what net edge would have been (for reporting)
            }

            // ── After: additionally gate on cost model ─────────────────────────
            if ap[i].is_none() {
                let exp_profit = compute_expected_profit(ne, POS_FRAC * aeq);
                if ne > 0.0 && exp_profit >= cost_cfg.min_expected_profit_usd {
                    ap[i] = Some((probs[i], tick, is_buy, aeq));
                    atrades += 1;
                    agross  += ge;
                    anet    += ne;
                } else {
                    rejected += 1;
                }
            }
        }
    }

    // ── Force-close all open positions at end of path ──────────────────────────
    for i in 0..N {
        if let Some((ep, _, buy, eeq)) = bp[i] {
            let pnl = if buy { probs[i] - ep } else { ep - probs[i] };
            beq += pnl * POS_FRAC * eeq;
        }
        if let Some((ep, _, buy, eeq)) = ap[i] {
            let pnl = if buy { probs[i] - ep } else { ep - probs[i] };
            aeq += pnl * POS_FRAC * eeq;
        }
    }

    // Final drawdown after force-close.
    if bpeak > 0.0 { bdd = bdd.max((bpeak - beq.min(bpeak)) / bpeak); }
    if apeak > 0.0 { add = add.max((apeak - aeq.min(apeak)) / apeak); }

    PathResult {
        before_return:    (beq - INITIAL_CAPITAL) / INITIAL_CAPITAL,
        after_return:     (aeq - INITIAL_CAPITAL) / INITIAL_CAPITAL,
        before_max_dd:    bdd,
        after_max_dd:     add,
        before_trades:    btrades,
        after_trades:     atrades,
        cost_rejected:    rejected,
        before_gross_sum: bgross,
        before_net_sum:   bnet,
        after_gross_sum:  agross,
        after_net_sum:    anet,
    }
}

// ── Statistics helpers ─────────────────────────────────────────────────────────

fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() { return 0.0; }
    xs.iter().sum::<f64>() / xs.len() as f64
}

fn stddev(xs: &[f64]) -> f64 {
    if xs.len() < 2 { return 0.0; }
    let m = mean(xs);
    let v = xs.iter().map(|&x| (x - m).powi(2)).sum::<f64>() / (xs.len() - 1) as f64;
    v.sqrt()
}

fn percentile_sorted(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() { return 0.0; }
    let idx = ((sorted.len() as f64 - 1.0) * p / 100.0).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

// ── Reporting helpers ──────────────────────────────────────────────────────────

fn row(label: &str, before: &str, after: &str, delta: &str) {
    println!("{:<44} {:>11} {:>11} {:>9}", label, before, after, delta);
}

fn sep(c: char, w: usize) {
    println!("{}", c.to_string().repeat(w));
}

// ── Main ───────────────────────────────────────────────────────────────────────

fn main() {
    let cost_cfg = CostModelConfig::default();

    eprintln!(
        "Running {} paths × {} ticks × {} markets…",
        NUM_PATHS, HORIZON, N
    );
    eprintln!(
        "Market: vol={:.1}%  corr={:.0}%  mean_rev={:.2}  hold={} ticks  pos={:.0}%",
        VOLATILITY * 100.0,
        CORRELATION * 100.0,
        MEAN_REVERSION,
        HOLD_TICKS,
        POS_FRAC * 100.0,
    );
    eprintln!(
        "Cost:   fee={:.0}%  spread_k={:.1}  slippage_k={}  decay={}/ms  latency={}ms  min_profit=${:.2}",
        cost_cfg.fee_rate * 100.0,
        cost_cfg.spread_volatility_k,
        cost_cfg.liquidity_impact_factor,
        cost_cfg.decay_rate,
        cost_cfg.expected_latency_ms,
        cost_cfg.min_expected_profit_usd,
    );
    eprintln!();

    // ── Run all paths in parallel ──────────────────────────────────────────────
    let results: Vec<PathResult> = (0..NUM_PATHS)
        .into_par_iter()
        .map(|id| run_path(id, &cost_cfg))
        .collect();

    let n = NUM_PATHS as f64;

    // ── Trade counts ───────────────────────────────────────────────────────────
    let btot: u64 = results.iter().map(|r| r.before_trades).sum();
    let atot: u64 = results.iter().map(|r| r.after_trades).sum();
    let rtot: u64 = results.iter().map(|r| r.cost_rejected).sum();

    let b_avg_trades = btot as f64 / n;
    let a_avg_trades = atot as f64 / n;

    // Candidates = signals the After strategy had an opportunity to take.
    let candidates = atot + rtot;
    let rej_rate   = if candidates > 0 { rtot as f64 / candidates as f64 } else { 0.0 };

    // ── Edge statistics ────────────────────────────────────────────────────────
    let bgross_total: f64 = results.iter().map(|r| r.before_gross_sum).sum();
    let bnet_total:   f64 = results.iter().map(|r| r.before_net_sum).sum();
    let agross_total: f64 = results.iter().map(|r| r.after_gross_sum).sum();
    let anet_total:   f64 = results.iter().map(|r| r.after_net_sum).sum();

    let b_avg_gross = if btot > 0 { bgross_total / btot as f64 } else { 0.0 };
    let b_avg_net   = if btot > 0 { bnet_total   / btot as f64 } else { 0.0 };
    let a_avg_gross = if atot > 0 { agross_total / atot as f64 } else { 0.0 };
    let a_avg_net   = if atot > 0 { anet_total   / atot as f64 } else { 0.0 };

    let b_eff = if b_avg_gross.abs() > 1e-12 { b_avg_net / b_avg_gross } else { 0.0 };
    let a_eff = if a_avg_gross.abs() > 1e-12 { a_avg_net / a_avg_gross } else { 0.0 };

    // ── Returns & Sharpe ───────────────────────────────────────────────────────
    let br: Vec<f64> = results.iter().map(|r| r.before_return).collect();
    let ar: Vec<f64> = results.iter().map(|r| r.after_return).collect();

    let b_mean_ret = mean(&br);
    let a_mean_ret = mean(&ar);
    let b_std_ret  = stddev(&br);
    let a_std_ret  = stddev(&ar);

    // Annualisation: each tick = 1 second; 252 trading days × 6.5 h × 3600 s.
    // Matches the convention used in simulation_engine/src/monte_carlo.rs.
    let ann_factor = (252.0 * 6.5 * 3_600.0 / HORIZON as f64).sqrt();
    let b_sharpe   = if b_std_ret > 0.0 { b_mean_ret / b_std_ret * ann_factor } else { 0.0 };
    let a_sharpe   = if a_std_ret > 0.0 { a_mean_ret / a_std_ret * ann_factor } else { 0.0 };

    // ── PnL ───────────────────────────────────────────────────────────────────
    let b_pnl_mean = b_mean_ret * INITIAL_CAPITAL;
    let a_pnl_mean = a_mean_ret * INITIAL_CAPITAL;

    // ── Profit probability ─────────────────────────────────────────────────────
    let b_pp = br.iter().filter(|&&r| r > 0.0).count() as f64 / n;
    let a_pp = ar.iter().filter(|&&r| r > 0.0).count() as f64 / n;

    // ── Drawdown ───────────────────────────────────────────────────────────────
    let bdd_vec: Vec<f64> = results.iter().map(|r| r.before_max_dd).collect();
    let add_vec: Vec<f64> = results.iter().map(|r| r.after_max_dd).collect();

    let b_dd_mean = mean(&bdd_vec);
    let a_dd_mean = mean(&add_vec);
    let b_dd_p95  = { let mut s = bdd_vec.clone(); s.sort_by(|a, b| a.partial_cmp(b).unwrap()); percentile_sorted(&s, 95.0) };
    let a_dd_p95  = { let mut s = add_vec.clone(); s.sort_by(|a, b| a.partial_cmp(b).unwrap()); percentile_sorted(&s, 95.0) };
    let b_dd_max  = bdd_vec.iter().cloned().fold(0.0_f64, f64::max);
    let a_dd_max  = add_vec.iter().cloned().fold(0.0_f64, f64::max);

    // ── VaR / CVaR (5 % tail) ─────────────────────────────────────────────────
    let mut br_sorted = br.clone(); br_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut ar_sorted = ar.clone(); ar_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let tail = (n * 0.05).round() as usize;
    let tail = tail.max(1);

    // 5th-percentile return (positive = profit; if both strategies are always
    // profitable this will be positive, so we keep it as a return not a loss).
    let b_p05   = percentile_sorted(&br_sorted, 5.0);
    let a_p05   = percentile_sorted(&ar_sorted, 5.0);
    // Expected return of the worst 5 % of paths (CVaR-equivalent).
    let b_exp05 = mean(&br_sorted[..tail]);
    let a_exp05 = mean(&ar_sorted[..tail]);

    // ── Print report ───────────────────────────────────────────────────────────
    let w = 77;
    println!();
    sep('═', w);
    println!("{:^width$}", "NET EDGE MODEL — BEFORE vs AFTER BACKTEST", width = w);
    println!("{:^width$}",
        format!("{NUM_PATHS} paths × {HORIZON} ticks × {N} markets  |  vol={:.1}%  corr={:.0}%  seed={SEED}",
            VOLATILITY * 100.0, CORRELATION * 100.0),
        width = w);
    sep('═', w);
    println!("{:<44} {:>11} {:>11} {:>9}", "Metric", "Before", "After", "Δ");
    sep('─', w);

    // Trade counts
    row("Avg trades per path",
        &format!("{:.1}", b_avg_trades),
        &format!("{:.1}", a_avg_trades),
        &format!("{:+.1}", a_avg_trades - b_avg_trades));
    row("Cost-filter rejection rate",
        "—",
        &format!("{:.1}%", rej_rate * 100.0),
        "—");
    row("Cost-filter rejections (per path)",
        "—",
        &format!("{:.1}", rtot as f64 / n),
        "—");

    sep('─', w);

    // PnL
    row("Expected PnL (mean path)",
        &format!("${:+.2}", b_pnl_mean),
        &format!("${:+.2}", a_pnl_mean),
        &format!("${:+.2}", a_pnl_mean - b_pnl_mean));
    row("Return vs initial capital",
        &format!("{:+.3}%", b_mean_ret * 100.0),
        &format!("{:+.3}%", a_mean_ret * 100.0),
        &format!("{:+.3}pp", (a_mean_ret - b_mean_ret) * 100.0));

    sep('─', w);

    // Edge quality
    row("Avg gross edge per trade",
        &format!("{:.3}%", b_avg_gross * 100.0),
        &format!("{:.3}%", a_avg_gross * 100.0),
        &format!("{:+.3}pp", (a_avg_gross - b_avg_gross) * 100.0));
    row("Avg net edge per trade",
        &format!("{:+.3}%", b_avg_net * 100.0),
        &format!("{:+.3}%", a_avg_net * 100.0),
        &format!("{:+.3}pp", (a_avg_net - b_avg_net) * 100.0));
    row("Edge efficiency  (net / gross)",
        &format!("{:+.1}%", b_eff * 100.0),
        &format!("{:+.1}%", a_eff * 100.0),
        &format!("{:+.1}pp", (a_eff - b_eff) * 100.0));

    sep('─', w);

    // Risk
    row("Sharpe ratio (annualised)",
        &format!("{:+.4}", b_sharpe),
        &format!("{:+.4}", a_sharpe),
        &format!("{:+.4}", a_sharpe - b_sharpe));
    row("Profit probability",
        &format!("{:.2}%", b_pp * 100.0),
        &format!("{:.2}%", a_pp * 100.0),
        &format!("{:+.2}pp", (a_pp - b_pp) * 100.0));
    row("Max drawdown (mean per path)",
        &format!("{:.3}%", b_dd_mean * 100.0),
        &format!("{:.3}%", a_dd_mean * 100.0),
        &format!("{:+.3}pp", (a_dd_mean - b_dd_mean) * 100.0));
    row("Max drawdown (p95 per path)",
        &format!("{:.3}%", b_dd_p95 * 100.0),
        &format!("{:.3}%", a_dd_p95 * 100.0),
        &format!("{:+.3}pp", (a_dd_p95 - b_dd_p95) * 100.0));
    row("Max drawdown (worst path)",
        &format!("{:.3}%", b_dd_max * 100.0),
        &format!("{:.3}%", a_dd_max * 100.0),
        &format!("{:+.3}pp", (a_dd_max - b_dd_max) * 100.0));
    row("5th-pct return (tail floor)",
        &format!("{:+.3}%", b_p05 * 100.0),
        &format!("{:+.3}%", a_p05 * 100.0),
        &format!("{:+.3}pp", (a_p05 - b_p05) * 100.0));
    row("Exp return in worst 5% paths",
        &format!("{:+.3}%", b_exp05 * 100.0),
        &format!("{:+.3}%", a_exp05 * 100.0),
        &format!("{:+.3}pp", (a_exp05 - b_exp05) * 100.0));

    sep('═', w);
    println!();

    // ── Cost model parameters reminder ────────────────────────────────────────
    println!("Cost model (default config):");
    println!("  fee_rate={:.0}%  spread_volatility_k={:.1}  liquidity_impact={} per pos-frac",
        cost_cfg.fee_rate * 100.0, cost_cfg.spread_volatility_k, cost_cfg.liquidity_impact_factor);
    println!("  decay_rate={}/ms  latency={}ms  min_expected_profit=${:.2}",
        cost_cfg.decay_rate, cost_cfg.expected_latency_ms, cost_cfg.min_expected_profit_usd);
    println!();

    // ── Cost floor ────────────────────────────────────────────────────────────
    // Show the implied cost floor (minimum gross_edge for a viable trade).
    // At typical params: fees + spread + decay(50ms) for a boundary signal.
    let floor_gross   = 0.035_f64;
    let floor_cost    = compute_cost_estimate(floor_gross, POS_FRAC, VOLATILITY,
                                              cost_cfg.expected_latency_ms, &cost_cfg);
    let floor_net     = compute_net_edge(floor_gross, &floor_cost);
    let floor_profit  = compute_expected_profit(floor_net, POS_FRAC * INITIAL_CAPITAL);
    println!("Break-even analysis (gross_edge = {:.1}%):", floor_gross * 100.0);
    println!("  fees={:.2}%  spread={:.2}%  slippage={:.4}%  decay={:.3}%  → total={:.3}%",
        floor_cost.fees * 100.0, floor_cost.spread * 100.0,
        floor_cost.slippage * 100.0, floor_cost.decay_cost * 100.0,
        floor_cost.total_cost * 100.0);
    println!("  net_edge={:+.3}%  expected_profit=${:+.2} on ${:.0} position",
        floor_net * 100.0, floor_profit, POS_FRAC * INITIAL_CAPITAL);
    println!();

    // ── Interpretation ────────────────────────────────────────────────────────
    println!("Interpretation:");
    let saved_pnl = a_pnl_mean - b_pnl_mean;
    let pct_trades_saved = (b_avg_trades - a_avg_trades) / b_avg_trades * 100.0;
    println!("  • Cost model filters {:.1}% of trades ({:.1} → {:.1} per path).",
        pct_trades_saved, b_avg_trades, a_avg_trades);
    println!("  • Mean-path PnL improves by ${:+.2} ({:+.3}% return).",
        saved_pnl, (a_mean_ret - b_mean_ret) * 100.0);
    println!("  • Edge efficiency: {:+.1}% before → {:+.1}% after (net/gross ratio).",
        b_eff * 100.0, a_eff * 100.0);
    println!("  • Sharpe: {:+.4} before → {:+.4} after (Δ {:+.4}).",
        b_sharpe, a_sharpe, a_sharpe - b_sharpe);
    println!("  • Profit probability: {:.2}% before → {:.2}% after (Δ {:+.2}pp).",
        b_pp * 100.0, a_pp * 100.0, (a_pp - b_pp) * 100.0);
    println!("  • Avg drawdown reduced by {:+.3}pp (mean per path).",
        (a_dd_mean - b_dd_mean) * 100.0);
}
