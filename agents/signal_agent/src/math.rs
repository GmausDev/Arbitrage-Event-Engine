// agents/signal_agent/src/math.rs
// Pure mathematical helpers for EV and Kelly sizing — no I/O, no side effects.

use common::TradeDirection;

// ---------------------------------------------------------------------------
// Expected value
// ---------------------------------------------------------------------------

/// Compute the expected value for each trade direction and return the dominant
/// one, or `None` if neither side clears zero after trading costs.
///
/// # Formulas
/// ```text
/// EV_buy  = posterior − market − trading_cost
/// EV_sell = market − posterior − trading_cost
/// ```
///
/// Direction is chosen by which side has positive EV:
/// - `posterior > market` → BUY candidate
/// - `market > posterior` → SELL candidate
///
/// If the net EV is ≤ 0 after costs, returns `None`.
pub fn compute_expected_value(
    posterior: f64,
    market: f64,
    trading_cost: f64,
) -> Option<(TradeDirection, f64)> {
    if posterior > market {
        let ev = posterior - market - trading_cost;
        if ev > 0.0 {
            return Some((TradeDirection::Buy, ev));
        }
    } else if market > posterior {
        let ev = market - posterior - trading_cost;
        if ev > 0.0 {
            return Some((TradeDirection::Sell, ev));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Kelly position sizing
// ---------------------------------------------------------------------------

/// Kelly-optimal position fraction for binary prediction market contracts.
///
/// Uses the closed-form solution for each side:
///
/// ```text
/// BUY  (p > m): f = (p − m) / (1 − m)
/// SELL (p < m): f = (m − p) / m
/// ```
///
/// These are derived from the standard Kelly formula `(b·p − q)/b` with the
/// payout odds appropriate to each side of the binary contract.
///
/// The result is:
/// 1. Clamped to `[0, max_position_fraction]`
/// 2. Scaled by `kelly_fraction` (fractional Kelly multiplier, e.g. 0.25)
///
/// Returns `0.0` for `TradeDirection::Arbitrage` (not handled here).
pub fn compute_kelly_fraction(
    posterior: f64,
    market: f64,
    direction: TradeDirection,
    max_position_fraction: f64,
    kelly_fraction: f64,
) -> f64 {
    let raw = match direction {
        TradeDirection::Buy => {
            let denom = 1.0 - market;
            if denom < 1e-12 {
                return 0.0;
            }
            (posterior - market) / denom
        }
        TradeDirection::Sell => {
            if market < 1e-12 {
                return 0.0;
            }
            (market - posterior) / market
        }
        TradeDirection::Arbitrage => return 0.0,
    };

    // Negative raw Kelly means the model is wrong about direction (shouldn't
    // happen since direction is chosen by EV sign), but guard anyway.
    (raw.max(0.0) * kelly_fraction).min(max_position_fraction)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;

    const EPS: f64 = 1e-10;

    // ── EV tests ──────────────────────────────────────────────────────────────

    #[test]
    fn ev_buy_spec_example() {
        // Spec example: posterior=0.63, market=0.54, cost=0.003 → EV=0.087
        let (dir, ev) = compute_expected_value(0.63, 0.54, 0.003).unwrap();
        assert_eq!(dir, TradeDirection::Buy);
        assert_abs_diff_eq!(ev, 0.087, epsilon = EPS);
    }

    #[test]
    fn ev_sell_direction() {
        // market > posterior → SELL
        let (dir, ev) = compute_expected_value(0.40, 0.60, 0.003).unwrap();
        assert_eq!(dir, TradeDirection::Sell);
        // EV_sell = 0.60 − 0.40 − 0.003 = 0.197
        assert_abs_diff_eq!(ev, 0.197, epsilon = EPS);
    }

    #[test]
    fn ev_cost_eats_edge_returns_none() {
        // Gap = 0.002, cost = 0.003 → net EV = −0.001 < 0
        assert!(compute_expected_value(0.502, 0.500, 0.003).is_none());
    }

    #[test]
    fn ev_small_positive_gap_above_cost_returns_some() {
        // Gap = 0.01, cost = 0.003 → net EV = 0.007 > 0
        assert!(compute_expected_value(0.51, 0.50, 0.003).is_some());
    }

    #[test]
    fn ev_equal_probs_returns_none() {
        assert!(compute_expected_value(0.50, 0.50, 0.0).is_none());
    }

    // ── Kelly tests ───────────────────────────────────────────────────────────

    #[test]
    fn kelly_buy_spec_example() {
        // p=0.63, m=0.54 → f = (0.63−0.54)/(1−0.54) = 0.09/0.46 ≈ 0.19565
        let f = compute_kelly_fraction(0.63, 0.54, TradeDirection::Buy, 1.0, 1.0);
        assert_abs_diff_eq!(f, 0.09 / 0.46, epsilon = EPS);
    }

    #[test]
    fn kelly_sell_correct_formula() {
        // p=0.40, m=0.60 → f = (0.60−0.40)/0.60 = 0.20/0.60 ≈ 0.3333
        // NOT the wrong YES-side formula (0.20/0.40 = 0.5)
        let f = compute_kelly_fraction(0.40, 0.60, TradeDirection::Sell, 1.0, 1.0);
        assert_abs_diff_eq!(f, 0.20 / 0.60, epsilon = EPS);
        // Confirm it differs significantly from the wrong formula
        assert!((f - 0.20 / 0.40).abs() > 0.1);
    }

    #[test]
    fn kelly_clamped_to_max_position() {
        // Very high edge should be clamped to max
        let f = compute_kelly_fraction(0.99, 0.01, TradeDirection::Buy, 0.05, 1.0);
        assert_abs_diff_eq!(f, 0.05, epsilon = EPS);
    }

    #[test]
    fn kelly_fractional_multiplier_applied() {
        // p=0.63, m=0.54, kelly_frac=0.25
        let raw = 0.09 / 0.46;
        let expected = (raw * 0.25_f64).min(1.0);
        let f = compute_kelly_fraction(0.63, 0.54, TradeDirection::Buy, 1.0, 0.25);
        assert_abs_diff_eq!(f, expected, epsilon = EPS);
    }

    #[test]
    fn kelly_fractional_and_clamped_spec_example() {
        // Spec example: posterior=0.63, market=0.54
        // raw kelly ≈ 0.1957, × 0.25 ≈ 0.0489 — fits under max_pos=0.05
        let f = compute_kelly_fraction(0.63, 0.54, TradeDirection::Buy, 0.05, 0.25);
        let expected = (0.09_f64 / 0.46 * 0.25).min(0.05);
        assert_abs_diff_eq!(f, expected, epsilon = EPS);
        assert!(f <= 0.05);
    }

    #[test]
    fn kelly_arbitrage_returns_zero() {
        let f = compute_kelly_fraction(0.70, 0.50, TradeDirection::Arbitrage, 0.05, 0.25);
        assert_abs_diff_eq!(f, 0.0, epsilon = EPS);
    }

    #[test]
    fn kelly_buy_near_certain_market_guard() {
        // market ≈ 1.0 → denom near zero → should return 0
        let f = compute_kelly_fraction(1.0, 1.0 - 1e-15, TradeDirection::Buy, 0.05, 0.25);
        assert_abs_diff_eq!(f, 0.0, epsilon = EPS);
    }

    #[test]
    fn kelly_sell_near_zero_market_guard() {
        // market ≈ 0 → division guard → should return 0
        let f = compute_kelly_fraction(0.0, 1e-15, TradeDirection::Sell, 0.05, 0.25);
        assert_abs_diff_eq!(f, 0.0, epsilon = EPS);
    }
}
