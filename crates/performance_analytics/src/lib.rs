// crates/performance_analytics/src/lib.rs
//
// Real-time and historical performance tracking for the Prediction Market Agent.
//
// # Pipeline position
//
// ```text
// PortfolioEngine
//       ↓  Event::Portfolio(PortfolioUpdate)
// PerformanceAnalytics              ← this crate
//       (also consumes Event::Market for per-position MTM)
// ```
//
// # Design
//
// All metric computation is pure (no async, no locks) and operates on
// `&PortfolioUpdate` / `&MarketUpdate` values.  The async `run()` loop only
// dispatches to these pure helpers, keeping tests simple and fast.

pub mod config;
pub mod edge_metrics;
pub mod engine;
pub mod types;

pub use config::AnalyticsConfig;
pub use edge_metrics::EdgeMetrics;
pub use engine::PerformanceAnalytics;
pub use types::{ExportFormat, PortfolioMetrics, RiskMetrics};
