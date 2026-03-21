// crates/cost_model/src/lib.rs
//
// Cost Estimation Model — net-edge computation for the Prediction Market Agent.
//
// # Role in the pipeline
//
// The cost model is a pure-computation library (no async, no I/O) used by the
// `risk_engine` as a hard gate in the trade approval pipeline.  For every
// incoming `TradeSignal` it:
//
//   1. Estimates fees, spread, slippage, and latency-driven edge decay.
//   2. Computes `net_edge = gross_edge − total_cost`.
//   3. Computes `expected_profit = net_edge × position_size_usd`.
//
// Signals with `net_edge ≤ 0` or `expected_profit < min_expected_profit_usd`
// are rejected before exposure bookkeeping.

pub mod config;
pub mod model;

pub use config::CostModelConfig;
pub use model::{
    compute_cost_estimate, compute_expected_profit, compute_net_edge,
    compute_net_edge_position_size, CostEstimate,
};
