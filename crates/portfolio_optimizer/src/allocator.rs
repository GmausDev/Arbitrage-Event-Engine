// crates/portfolio_optimizer/src/allocator.rs
//
// Pure, synchronous allocation algorithm.  No I/O, no async — easy to test
// and to call from both the live event loop and backtesting harnesses.

use std::collections::HashMap;

use common::{OptimizedSignal, TradeSignal};

use crate::config::AllocationConfig;

// ── Public input / output types ───────────────────────────────────────────────

/// Read-only view of portfolio state needed by the allocator.
pub struct AllocationInput<'a> {
    pub config:            &'a AllocationConfig,
    /// Current open positions (market_id → deployed fraction of capital).
    pub positions:         &'a HashMap<String, f64>,
    /// Fraction of total capital that is free to deploy, in `[0.0, 1.0]`.
    pub capital_available: f64,
}

/// Result of one allocation pass.
pub struct AllocationOutput {
    /// Optimized signals ready to publish, in allocation-descending order.
    pub optimized: Vec<OptimizedSignal>,
}

// ── Core algorithm ────────────────────────────────────────────────────────────

/// Compute optimal per-signal allocations for a batch of raw `TradeSignal`s.
///
/// Steps
/// -----
/// 1. **Dedup** — for each `market_id` keep only the signal with the highest
///    risk-adjusted expected value (`ev × confidence`).
/// 2. **Rank** — sort surviving signals by RAEV descending.
/// 3. **Greedy allocation** — iterate in rank order, applying:
///    - Per-market cap (`max_allocation_per_market`)
///    - Per-niche cap (`max_allocation_per_niche`)
///    - Total-capital cap (`total_capital × capital_available`)
///    - Intra-cluster correlation penalty (attenuates allocation as the niche
///      fills up, using `correlation_penalty_factor × default_cluster_correlation
///      × niche_utilization`)
/// 4. Signals with a resulting allocation of `0.0` are dropped.
///
/// The function is deterministic and allocates in O(n log n) time.
pub fn compute_allocations(signals: Vec<TradeSignal>, input: &AllocationInput<'_>) -> AllocationOutput {
    let config = input.config;
    let budget = config.total_capital * input.capital_available;

    // ── Step 1: dedup — keep highest RAEV per market ──────────────────────────
    let mut best: HashMap<String, (TradeSignal, f64)> = HashMap::new();
    for signal in signals {
        let raev = signal.expected_value * signal.confidence;
        // Discard signals with non-finite RAEV (NaN/±Inf from corrupted upstream).
        if !raev.is_finite() {
            continue;
        }
        let entry = best.entry(signal.market_id.clone()).or_insert_with(|| (signal.clone(), raev));
        if raev > entry.1 {
            *entry = (signal, raev);
        }
    }

    // ── Step 2: rank by RAEV descending ───────────────────────────────────────
    let mut ranked: Vec<(TradeSignal, f64)> = best.into_values().collect();
    ranked.sort_by(|(_, ra), (_, rb)| rb.partial_cmp(ra).unwrap_or(std::cmp::Ordering::Equal));

    // ── Step 3: greedy allocation ──────────────────────────────────────────────
    let mut total_used: f64 = 0.0;
    let mut niche_used: HashMap<String, f64> = HashMap::new();
    let mut optimized: Vec<OptimizedSignal> = Vec::with_capacity(ranked.len());

    for (signal, raev) in ranked {
        let cluster = cluster_id(&signal.market_id).to_string();

        let niche_so_far = *niche_used.get(&cluster).unwrap_or(&0.0);
        let niche_util   = if config.max_allocation_per_niche > 0.0 {
            niche_so_far / config.max_allocation_per_niche
        } else {
            1.0
        };

        // Correlation penalty: scales linearly with how much of the niche is used.
        let corr_penalty  = config.correlation_penalty_factor
            * config.default_cluster_correlation
            * niche_util;
        let penalty_scale = (1.0 - corr_penalty).max(0.0);

        // Effective caps after penalties.
        let cap_market = config.max_allocation_per_market;
        let cap_niche  = (config.max_allocation_per_niche - niche_so_far).max(0.0);
        let cap_total  = (budget - total_used).max(0.0);

        // Final size: take signal's requested fraction, apply caps, then penalty.
        let raw       = signal.position_fraction;
        let mut alloc = raw.min(cap_market).min(cap_niche).min(cap_total);
        alloc        *= penalty_scale;

        // Reject non-finite values (NaN from corrupted inputs) and genuine zeros.
        if !alloc.is_finite() || alloc <= 0.0 {
            continue;
        }

        // Build optimized signal: clone original, replace position_fraction.
        let mut opt_signal          = signal.clone();
        opt_signal.position_fraction = alloc;

        optimized.push(OptimizedSignal {
            signal:             opt_signal,
            risk_adjusted_ev:   raev,
            source_strategies:  vec![],   // populated by orchestrator if needed
            correlation_penalty: corr_penalty,
            niche_utilization:  niche_util,
        });

        total_used                             += alloc;
        *niche_used.entry(cluster).or_insert(0.0) += alloc;
    }

    AllocationOutput { optimized }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Extract the cluster/niche prefix from a market ID.
///
/// `"US_ELECTION_TRUMP"` → `"US"`;  `"BTCUSD"` → `"BTCUSD"`.
#[inline]
fn cluster_id(market_id: &str) -> &str {
    market_id.split('_').next().unwrap_or(market_id)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use common::TradeDirection;

    fn make_signal(market_id: &str, ev: f64, confidence: f64, pos_frac: f64) -> TradeSignal {
        TradeSignal {
            market_id:        market_id.to_string(),
            direction:        TradeDirection::Buy,
            expected_value:   ev,
            position_fraction: pos_frac,
            posterior_prob:   0.60,
            market_prob:      0.50,
            confidence,
            timestamp:        Utc::now(),
            source:           String::new(),
        }
    }

    fn default_input(config: &AllocationConfig) -> AllocationInput<'_> {
        static EMPTY: std::sync::OnceLock<HashMap<String, f64>> = std::sync::OnceLock::new();
        AllocationInput { config, positions: EMPTY.get_or_init(HashMap::new), capital_available: 1.0 }
    }

    // ── 1. Single signal within all caps ──────────────────────────────────────

    #[test]
    fn single_signal_within_caps() {
        let config = AllocationConfig::default();
        let signals = vec![make_signal("BTCUSD", 0.05, 0.80, 0.08)];
        let out = compute_allocations(signals, &default_input(&config));
        assert_eq!(out.optimized.len(), 1);
        // position_fraction (0.08) < max_allocation_per_market (0.10)
        // niche util = 0 → penalty = 0 → alloc = 0.08
        assert!((out.optimized[0].signal.position_fraction - 0.08).abs() < 1e-9);
    }

    // ── 2. Per-market cap enforced ─────────────────────────────────────────────

    #[test]
    fn per_market_cap_enforced() {
        let config = AllocationConfig::default(); // max_allocation_per_market = 0.10
        let signals = vec![make_signal("BTCUSD", 0.05, 0.80, 0.20)]; // requests 20 %
        let out = compute_allocations(signals, &default_input(&config));
        assert_eq!(out.optimized.len(), 1);
        assert!((out.optimized[0].signal.position_fraction - 0.10).abs() < 1e-9,
            "expected 0.10, got {}", out.optimized[0].signal.position_fraction);
    }

    // ── 3. Total capital cap enforced ─────────────────────────────────────────

    #[test]
    fn total_capital_cap_enforced() {
        let config = AllocationConfig { total_capital: 0.15, ..Default::default() };
        // Two signals each requesting 0.10, but budget = 0.15
        let signals = vec![
            make_signal("AAPL", 0.08, 0.90, 0.10),  // RAEV = 0.072 → allocated first
            make_signal("TSLA", 0.05, 0.70, 0.10),  // RAEV = 0.035 → only 0.05 left
        ];
        let out = compute_allocations(signals, &default_input(&config));
        assert_eq!(out.optimized.len(), 2);
        let total: f64 = out.optimized.iter().map(|s| s.signal.position_fraction).sum();
        assert!(total <= 0.15 + 1e-9, "total {total} exceeds budget 0.15");
    }

    // ── 4. Dedup keeps highest RAEV ───────────────────────────────────────────

    #[test]
    fn dedup_keeps_highest_raev() {
        let config = AllocationConfig::default();
        let signals = vec![
            make_signal("BTCUSD", 0.03, 0.50, 0.05),  // RAEV = 0.015
            make_signal("BTCUSD", 0.10, 0.80, 0.08),  // RAEV = 0.080 — winner
        ];
        let out = compute_allocations(signals, &default_input(&config));
        assert_eq!(out.optimized.len(), 1);
        assert!((out.optimized[0].risk_adjusted_ev - 0.08).abs() < 1e-9);
    }

    // ── 5. Intra-cluster correlation penalty applied ───────────────────────────

    #[test]
    fn intra_cluster_penalty_reduces_second_allocation() {
        let config = AllocationConfig {
            max_allocation_per_niche:    0.20,
            correlation_penalty_factor:  1.0,   // max penalty to make effect measurable
            default_cluster_correlation: 0.5,
            max_allocation_per_market:   0.10,
            total_capital:               1.0,
            ..Default::default()
        };
        let signals = vec![
            make_signal("US_ELECTION_TRUMP",  0.10, 0.90, 0.10), // RAEV = 0.09 → 1st, niche 0 → no penalty
            make_signal("US_ELECTION_HARRIS", 0.08, 0.80, 0.10), // RAEV = 0.064 → 2nd, niche = 0.10/0.20 = 0.5
        ];
        let out = compute_allocations(signals, &default_input(&config));
        assert_eq!(out.optimized.len(), 2);

        let first  = out.optimized.iter().find(|s| s.signal.market_id == "US_ELECTION_TRUMP").unwrap();
        let second = out.optimized.iter().find(|s| s.signal.market_id == "US_ELECTION_HARRIS").unwrap();

        // First: no penalty applied
        assert!((first.correlation_penalty).abs() < 1e-9);
        // Second: penalty > 0, so allocation < raw request (0.10)
        assert!(second.signal.position_fraction < 0.10,
            "expected penalty to reduce allocation, got {}", second.signal.position_fraction);
    }

    // ── 6. Per-niche cap limits aggregate ─────────────────────────────────────

    #[test]
    fn per_niche_cap_limits_aggregate() {
        let config = AllocationConfig {
            max_allocation_per_niche: 0.15,
            correlation_penalty_factor: 0.0,  // disable penalty to isolate niche cap
            max_allocation_per_market: 0.10,
            total_capital: 1.0,
            ..Default::default()
        };
        // Two US markets each requesting 0.10, niche cap = 0.15
        let signals = vec![
            make_signal("US_MARKET_A", 0.09, 0.90, 0.10),
            make_signal("US_MARKET_B", 0.07, 0.80, 0.10),
        ];
        let out = compute_allocations(signals, &default_input(&config));
        let niche_total: f64 = out.optimized.iter().map(|s| s.signal.position_fraction).sum();
        assert!(niche_total <= 0.15 + 1e-9,
            "niche aggregate {} exceeds cap 0.15", niche_total);
    }

    // ── 7. Empty input produces empty output ──────────────────────────────────

    #[test]
    fn empty_input_empty_output() {
        let config = AllocationConfig::default();
        let out = compute_allocations(vec![], &default_input(&config));
        assert!(out.optimized.is_empty());
    }

    // ── 8. RAEV ranking determines allocation priority ────────────────────────

    #[test]
    fn raev_ranking_determines_priority() {
        // Budget forces one signal to be sized down
        let config = AllocationConfig {
            total_capital:             0.12,
            max_allocation_per_market: 0.10,
            ..Default::default()
        };
        let signals = vec![
            make_signal("LOW_EV",  0.02, 0.50, 0.10),  // RAEV = 0.010
            make_signal("HIGH_EV", 0.10, 0.90, 0.10),  // RAEV = 0.090 → gets full 0.10
        ];
        let out = compute_allocations(signals, &default_input(&config));
        let high = out.optimized.iter().find(|s| s.signal.market_id == "HIGH_EV").unwrap();
        let low  = out.optimized.iter().find(|s| s.signal.market_id == "LOW_EV").unwrap();
        // HIGH_EV allocated 0.10; LOW_EV can only get 0.02 remaining
        assert!((high.signal.position_fraction - 0.10).abs() < 1e-9);
        assert!((low.signal.position_fraction  - 0.02).abs() < 1e-9);
    }
}
