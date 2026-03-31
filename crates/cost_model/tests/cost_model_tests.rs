// crates/cost_model/tests/cost_model_tests.rs

use cost_model::{
    compute_cost_estimate, compute_expected_profit, compute_net_edge,
    compute_net_edge_position_size, CostModelConfig,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn default_config() -> CostModelConfig {
    CostModelConfig::default()
}

fn approx_eq(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() < tol
}

// ── CM-1: Basic cost breakdown with default config ────────────────────────────

#[test]
fn cm1_basic_cost_breakdown() {
    let cfg = default_config();
    // gross_edge=0.10, position_fraction=0.05, volatility=0.02, latency=50ms
    let cost = compute_cost_estimate(0.10, 0.05, 0.02, 50.0, &cfg);

    // fees = 0.02
    assert!(approx_eq(cost.fees, 0.02, 1e-10), "fees={}", cost.fees);

    // spread = 0.5 * 0.02 = 0.01
    assert!(approx_eq(cost.spread, 0.01, 1e-10), "spread={}", cost.spread);

    // slippage = 0.05 * 0.001 = 0.00005
    assert!(approx_eq(cost.slippage, 0.00005, 1e-10), "slippage={}", cost.slippage);

    // decay_cost = 0.10 * (1 - exp(-0.001*50)) = 0.10 * (1 - exp(-0.05))
    let expected_decay = 0.10 * (1.0 - (-0.05_f64).exp());
    assert!(
        approx_eq(cost.decay_cost, expected_decay, 1e-10),
        "decay_cost={} expected={}",
        cost.decay_cost,
        expected_decay
    );

    // total_cost = fees + spread + slippage + decay_cost
    let expected_total = cost.fees + cost.spread + cost.slippage + cost.decay_cost;
    assert!(approx_eq(cost.total_cost, expected_total, 1e-10), "total_cost mismatch");
}

// ── CM-2: Positive net edge — trade should proceed ────────────────────────────

#[test]
fn cm2_positive_net_edge() {
    let cfg = default_config();
    let cost = compute_cost_estimate(0.10, 0.05, 0.02, 50.0, &cfg);
    let net = compute_net_edge(0.10, &cost);
    assert!(net > 0.0, "expected positive net_edge, got {net}");
    // net_edge < gross_edge
    assert!(net < 0.10, "net_edge should be less than gross_edge");
}

// ── CM-3: Negative net edge — small gross edge below cost floor ───────────────

#[test]
fn cm3_negative_net_edge_small_gross() {
    let cfg = default_config();
    // gross_edge=0.03 — fees alone are 0.02, spread adds 0.01 → total > 0.03
    let cost = compute_cost_estimate(0.03, 0.05, 0.02, 50.0, &cfg);
    let net = compute_net_edge(0.03, &cost);
    assert!(net < 0.0, "expected negative net_edge, got {net}");
}

// ── CM-4: Zero gross edge → net edge is negative ─────────────────────────────

#[test]
fn cm4_zero_gross_edge() {
    let cfg = default_config();
    let cost = compute_cost_estimate(0.0, 0.05, 0.02, 50.0, &cfg);
    let net = compute_net_edge(0.0, &cost);
    // fees + spread + slippage push net below zero
    assert!(net < 0.0, "net_edge should be negative when gross_edge=0");
    // decay_cost should be 0 when gross_edge=0
    assert!(approx_eq(cost.decay_cost, 0.0, 1e-10), "decay_cost should be 0");
}

// ── CM-5: Zero latency → no decay cost ───────────────────────────────────────

#[test]
fn cm5_zero_latency_no_decay() {
    let cfg = default_config();
    let cost = compute_cost_estimate(0.10, 0.05, 0.02, 0.0, &cfg);
    assert!(approx_eq(cost.decay_cost, 0.0, 1e-10), "decay_cost should be 0 at zero latency");
}

// ── CM-6: High latency → large decay cost ────────────────────────────────────

#[test]
fn cm6_high_latency_large_decay() {
    let cfg = default_config();
    // latency=5000ms → decay_rate*latency = 5.0 → nearly full edge decays
    let cost_hi = compute_cost_estimate(0.10, 0.05, 0.02, 5000.0, &cfg);
    let cost_lo = compute_cost_estimate(0.10, 0.05, 0.02, 50.0, &cfg);
    assert!(
        cost_hi.decay_cost > cost_lo.decay_cost,
        "higher latency should produce more decay"
    );
    // At decay_rate=0.001, latency=5000: decay ≈ 0.10*(1-e^-5) ≈ 0.0933
    assert!(cost_hi.decay_cost > 0.09, "decay_cost should be near full gross_edge at 5000ms");
}

// ── CM-7: Expected profit calculation ────────────────────────────────────────

#[test]
fn cm7_expected_profit() {
    // net_edge=0.05, position_size=$1000 → expected_profit=$50
    let profit = compute_expected_profit(0.05, 1_000.0);
    assert!(approx_eq(profit, 50.0, 1e-10), "profit={}", profit);
}

// ── CM-8: Expected profit negative when net_edge negative ────────────────────

#[test]
fn cm8_expected_profit_negative_edge() {
    let profit = compute_expected_profit(-0.02, 500.0);
    assert!(profit < 0.0, "profit should be negative when edge is negative");
}

// ── CM-9: Position scaling by net-to-gross ratio ─────────────────────────────

#[test]
fn cm9_position_scaling() {
    // gross=0.10, net=0.06 → ratio=0.6 → scaled = base * 0.6
    let scaled = compute_net_edge_position_size(0.05, 0.06, 0.10);
    assert!(approx_eq(scaled, 0.03, 1e-10), "scaled={}", scaled);
}

// ── CM-10: Position scaling clamped — net_edge ≥ gross_edge → no reduction ───

#[test]
fn cm10_position_scaling_no_reduction_above_1() {
    // net_edge > gross_edge (unusual, but should not inflate position)
    let scaled = compute_net_edge_position_size(0.05, 0.12, 0.10);
    assert!(approx_eq(scaled, 0.05, 1e-10), "scaled should equal base when ratio>1, got {}", scaled);
}

// ── CM-11: Position scaling when gross_edge=0 → returns 0 ───────────────────

#[test]
fn cm11_position_scaling_zero_gross() {
    let scaled = compute_net_edge_position_size(0.05, -0.01, 0.0);
    assert!(approx_eq(scaled, 0.0, 1e-10), "scaled should be 0 when gross_edge=0");
}

// ── CM-12: Custom config changes cost profile ─────────────────────────────────

#[test]
fn cm12_custom_config() {
    let cfg = CostModelConfig {
        fee_rate:                0.01,   // lower fee
        exchange_fees:           Default::default(),
        spread_volatility_k:     0.25,
        liquidity_impact_factor: 0.0005,
        decay_rate:              0.0005,
        expected_latency_ms:     50.0,
        min_expected_profit_usd: 0.25,
        default_volatility:      0.02,
        default_market_liquidity: 5_000.0,
    };
    let cfg_default = default_config();

    let cost_custom = compute_cost_estimate(0.08, 0.04, 0.02, 50.0, &cfg);
    let cost_default = compute_cost_estimate(0.08, 0.04, 0.02, 50.0, &cfg_default);

    assert!(
        cost_custom.total_cost < cost_default.total_cost,
        "custom lower-fee config should have lower total_cost"
    );
}

// ── CM-13: Net edge is clamped to [-1, 1] ────────────────────────────────────

#[test]
fn cm13_net_edge_clamp() {
    let cfg = CostModelConfig {
        fee_rate: 2.0,  // absurdly high fee to force total_cost >> gross_edge
        ..CostModelConfig::default()
    };
    let cost = compute_cost_estimate(0.05, 0.05, 0.02, 50.0, &cfg);
    let net = compute_net_edge(0.05, &cost);
    assert!(net >= -1.0, "net_edge should be clamped to -1.0, got {net}");
}

// ── CM-14: Spread scales with volatility ─────────────────────────────────────

#[test]
fn cm14_spread_scales_with_volatility() {
    let cfg = default_config();
    let cost_lo = compute_cost_estimate(0.10, 0.05, 0.01, 50.0, &cfg);
    let cost_hi = compute_cost_estimate(0.10, 0.05, 0.05, 50.0, &cfg);
    assert!(cost_hi.spread > cost_lo.spread, "higher volatility should produce higher spread");
    // spread_hi = 0.5*0.05 = 0.025, spread_lo = 0.5*0.01 = 0.005
    assert!(approx_eq(cost_hi.spread, 0.025, 1e-10));
    assert!(approx_eq(cost_lo.spread, 0.005, 1e-10));
}

// ── CM-15: Slippage scales with position_fraction ────────────────────────────

#[test]
fn cm15_slippage_scales_with_position() {
    let cfg = default_config();
    let cost_small = compute_cost_estimate(0.10, 0.01, 0.02, 50.0, &cfg);
    let cost_large = compute_cost_estimate(0.10, 0.10, 0.02, 50.0, &cfg);
    assert!(cost_large.slippage > cost_small.slippage, "larger position should have more slippage");
    assert!(approx_eq(cost_large.slippage, 0.10 * 0.001, 1e-10));
    assert!(approx_eq(cost_small.slippage, 0.01 * 0.001, 1e-10));
}
