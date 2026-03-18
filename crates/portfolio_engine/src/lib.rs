// crates/portfolio_engine/src/lib.rs
//
// Authoritative portfolio tracker.
//
// Subscribes to: Event::Execution(ExecutionResult)
// Publishes:     Event::Portfolio(PortfolioUpdate)

pub mod config;
pub mod engine;
pub mod state;
pub mod types;

pub use config::PortfolioConfig;
pub use engine::PortfolioEngine;
pub use state::{PortfolioState, SharedPortfolioState};
pub use types::PortfolioPosition;
