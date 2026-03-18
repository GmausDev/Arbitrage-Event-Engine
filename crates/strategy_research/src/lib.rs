// crates/strategy_research/src/lib.rs
//
// Automated Hypothesis Generation and Testing Loop.
//
// Subsystems (each in its own module):
//   hypothesis  — random parameter sampling + template permutations
//   templates   — reusable strategy building blocks
//   compiler    — Hypothesis → GeneratedStrategy with evaluate()
//   backtest    — synthetic market simulation + trade tracking
//   evaluator   — Sharpe, drawdown, win-rate from BacktestResult
//   registry    — DiscoveredStrategy store with capacity eviction
//   engine      — async event loop: timer → generate → backtest → promote

pub mod backtest;
pub mod compiler;
pub mod config;
pub mod engine;
pub mod evaluator;
pub mod hypothesis;
pub mod registry;
pub mod templates;
pub mod types;

pub use config::ResearchConfig;
pub use engine::StrategyResearchEngine;
pub use registry::StrategyRegistry;
pub use types::{DiscoveredStrategy, GeneratedStrategy, Hypothesis, StrategyMetrics};
