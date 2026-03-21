// agents/signal_agent/src/math.rs
// Pure mathematical helpers for EV, Kelly sizing, and liquidity scaling.
// No I/O, no side effects.

use common::TradeDirection;

// ---------------------------------------------------------------------------
// Expected value
// ---------------------------------------------------------------------------

/// Compute the expected value for each trade direction and return the dominant
/// one, or `None` if neither side clears zero after trading costs.
///
/// When `bid`/`ask` are provided the entry price is the real transaction cost:
/// - BUY  → pay `ask`  per contract → `EV = posterior − ask  − trading_cost`
/// - SELL → receive `bid` per contract → `EV = bid  − posterior − trading_cost`
///
/// When spread data is unavailable (or the spread is invalid) the mid `market`
/// is used as a fallback, preserving backwards-compatibility.
///
/// Returns `(direction, expected_value, entry_price)` where `entry_price` is
/// the price actually used for EV and Kelly sizing (`ask`, `bid`, or `market`).
pub fn compute_expected_value(
    posterior:    f64,
    market:       f64,       // mid price — used as fallback when spread absent
    bid:          Option<f64>,
    ask:          Option<f64>,
    trading_cost: f64,
) -> Option<(TradeDirection, f64, f64)> {
    // Resolve effective entry prices, validating the spread when both sides present.
    let (eff_bid, eff_ask) = match (bid, ask) {
        (Some(b), Some(a)) if a > b && b > 0.0 && a < 1.0 => (b, a),
        _ => (market, market), // invalid or absent spread → fall back to mid
    };

    if posterior > eff_ask {
        let ev = posterior - eff_ask - trading_cost;
        if ev > 0.0 {
            return Some((TradeDirection::Buy, ev, eff_ask));
        }
    } else if eff_bid > posterior {
        let ev = eff_bid - posterior - trading_cost;
        if ev > 0.0 {
            return Some((TradeDirection::Sell, ev, eff_bid));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Kelly position sizing
// ---------------------------------------------------------------------------

/// Kelly-optimal position fraction for binary prediction market contracts.
///
/// Uses the closed-form solution for each side, with `entry_price` as the
/// actual transaction price (ask for BUY, bid for SELL, or mid as fallback):
///
/// ```text
/// BUY  (posterior > entry_price): f = (posterior − entry_price) / (1 − entry_price)
/// SELL (entry_price > posterior): f = (entry_price − posterior) / entry_price
/// ```
///
/// The result is:
/// 1. Clamped to `[0, max_position_fraction]`
/// 2. Scaled by `kelly_fraction` (fractional Kelly multiplier, e.g. 0.50)
///
/// Returns `0.0` for `TradeDirection::Arbitrage` (not handled here).
pub fn compute_kelly_fraction(
    posterior:             f64,
    entry_price:           f64,
    direction:             TradeDirection,
    max_position_fraction: f64,
    kelly_fraction:        f64,
) -> f64 {
    let raw = match direction {
        TradeDirection::Buy => {
            let denom = 1.0 - entry_price;
            if denom < 1e-12 { return 0.0; }
            (posterior - entry_price) / denom
        }
        TradeDirection::Sell => {
            if entry_price < 1e-12 { return 0.0; }
            (entry_price - posterior) / entry_price
        }
        TradeDirection::Arbitrage => return 0.0,
    };

    // Negative raw Kelly means the model is wrong about direction (shouldn't
    // happen since direction is chosen by EV sign), but guard anyway.
    (raw.max(0.0) * kelly_fraction).min(max_position_fraction)
}

// ---------------------------------------------------------------------------
// Liquidity scaling
// ---------------------------------------------------------------------------

/// Scale a Kelly position fraction down when the market is illiquid.
///
/// Returns a multiplier in `[0.0, 1.0]`:
/// - `liquidity >= bankroll × threshold` → 1.0 (no scaling)
/// - `liquidity == 0`                     → 0.0 (no trade)
/// - Between: linear interpolation
///
/// This prevents deploying capital that would move the market against us.
pub fn compute_liquidity_scale(
    liquidity:           f64,
    bankroll:            f64,
    threshold_fraction:  f64,
) -> f64 {
    let threshold = bankroll * threshold_fraction;
    if threshold <= 0.0 { return 1.0; }
    (liquidity / threshold).min(1.0).max(0.0)
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
    fn ev_buy_spec_example_no_spread() {
        // Without spread: posterior=0.63, market=0.54, cost=0.003 → EV=0.087
        let (dir, ev, entry) = compute_expected_value(0.63, 0.54, None, None, 0.003).unwrap();
        assert_eq!(dir, TradeDirection::Buy);
        assert_abs_diff_eq!(ev, 0.087, epsilon = EPS);
        assert_abs_diff_eq!(entry, 0.54, epsilon = EPS);
    }

    #[test]
    fn ev_buy_uses_ask_when_spread_present() {
        // ask=0.56, posterior=0.63 → EV = 0.63 − 0.56 − 0.003 = 0.067
        let (dir, ev, entry) = compute_expected_value(0.63, 0.54, Some(0.52), Some(0.56), 0.003).unwrap();
        assert_eq!(dir, TradeDirection::Buy);
        assert_abs_diff_eq!(ev, 0.63 - 0.56 - 0.003, epsilon = 0.01);
        assert_abs_diff_eq!(entry, 0.56, epsilon = EPS);
    }

    #[test]
    fn ev_sell_uses_bid_when_spread_present() {
        // bid=0.38, posterior=0.40, market=0.60 → EV = 0.38 − 0.40 − 0.003 < 0 → None
        // bid=0.44, posterior=0.40 → EV = 0.44 − 0.40 − 0.003 = 0.037
        let (dir, ev, entry) = compute_expected_value(0.40, 0.60, Some(0.44), Some(0.56), 0.003).unwrap();
        assert_eq!(dir, TradeDirection::Sell);
        assert_abs_diff_eq!(ev, 0.44 - 0.40 - 0.003, epsilon = EPS);
        assert_abs_diff_eq!(entry, 0.44, epsilon = EPS);
    }

    #[test]
    fn ev_invalid_spread_falls_back_to_mid() {
        // ask < bid → invalid spread → fall back to mid
        let (dir, ev, entry) = compute_expected_value(0.63, 0.54, Some(0.56), Some(0.52), 0.003).unwrap();
        assert_eq!(dir, TradeDirection::Buy);
        assert_abs_diff_eq!(entry, 0.54, epsilon = EPS);
        assert_abs_diff_eq!(ev, 0.087, epsilon = EPS);
    }

    #[test]
    fn ev_sell_direction_no_spread() {
        // market > posterior → SELL
        let (dir, ev, _) = compute_expected_value(0.40, 0.60, None, None, 0.003).unwrap();
        assert_eq!(dir, TradeDirection::Sell);
        // EV_sell = 0.60 − 0.40 − 0.003 = 0.197
        assert_abs_diff_eq!(ev, 0.197, epsilon = EPS);
    }

    #[test]
    fn ev_cost_eats_edge_returns_none() {
        // Gap = 0.002, cost = 0.003 → net EV = −0.001 < 0
        assert!(compute_expected_value(0.502, 0.500, None, None, 0.003).is_none());
    }

    #[test]
    fn ev_small_positive_gap_above_cost_returns_some() {
        // Gap = 0.01, cost = 0.003 → net EV = 0.007 > 0
        assert!(compute_expected_value(0.51, 0.50, None, None, 0.003).is_some());
    }

    #[test]
    fn ev_equal_probs_returns_none() {
        assert!(compute_expected_value(0.50, 0.50, None, None, 0.0).is_none());
    }

    // ── Kelly tests ───────────────────────────────────────────────────────────

    #[test]
    fn kelly_buy_spec_example() {
        // p=0.63, entry=0.54 → f = (0.63−0.54)/(1−0.54) = 0.09/0.46 ≈ 0.19565
        let f = compute_kelly_fraction(0.63, 0.54, TradeDirection::Buy, 1.0, 1.0);
        assert_abs_diff_eq!(f, 0.09 / 0.46, epsilon = EPS);
    }

    #[test]
    fn kelly_sell_correct_formula() {
        // p=0.40, entry=0.60 → f = (0.60−0.40)/0.60 = 0.20/0.60 ≈ 0.3333
        let f = compute_kelly_fraction(0.40, 0.60, TradeDirection::Sell, 1.0, 1.0);
        assert_abs_diff_eq!(f, 0.20 / 0.60, epsilon = EPS);
        assert!((f - 0.20 / 0.40).abs() > 0.1);
    }

    #[test]
    fn kelly_clamped_to_max_position() {
        let f = compute_kelly_fraction(0.99, 0.01, TradeDirection::Buy, 0.05, 1.0);
        assert_abs_diff_eq!(f, 0.05, epsilon = EPS);
    }

    #[test]
    fn kelly_fractional_multiplier_applied() {
        let raw = 0.09 / 0.46;
        let expected = (raw * 0.25_f64).min(1.0);
        let f = compute_kelly_fraction(0.63, 0.54, TradeDirection::Buy, 1.0, 0.25);
        assert_abs_diff_eq!(f, expected, epsilon = EPS);
    }

    #[test]
    fn kelly_fractional_and_clamped_spec_example() {
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
        let f = compute_kelly_fraction(1.0, 1.0 - 1e-15, TradeDirection::Buy, 0.05, 0.25);
        assert_abs_diff_eq!(f, 0.0, epsilon = EPS);
    }

    #[test]
    fn kelly_sell_near_zero_market_guard() {
        let f = compute_kelly_fraction(0.0, 1e-15, TradeDirection::Sell, 0.05, 0.25);
        assert_abs_diff_eq!(f, 0.0, epsilon = EPS);
    }

    // ── Liquidity scale tests ─────────────────────────────────────────────────

    #[test]
    fn liquidity_scale_full_when_above_threshold() {
        // bankroll=10_000, threshold=0.1 → threshold_usd=1_000
        // liquidity=2_000 >= 1_000 → scale = 1.0
        let s = compute_liquidity_scale(2_000.0, 10_000.0, 0.1);
        assert_abs_diff_eq!(s, 1.0, epsilon = EPS);
    }

    #[test]
    fn liquidity_scale_proportional_below_threshold() {
        // liquidity=500, threshold_usd=1_000 → scale = 0.5
        let s = compute_liquidity_scale(500.0, 10_000.0, 0.1);
        assert_abs_diff_eq!(s, 0.5, epsilon = EPS);
    }

    #[test]
    fn liquidity_scale_zero_when_no_liquidity() {
        let s = compute_liquidity_scale(0.0, 10_000.0, 0.1);
        assert_abs_diff_eq!(s, 0.0, epsilon = EPS);
    }
}
