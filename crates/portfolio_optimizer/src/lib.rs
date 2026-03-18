// crates/portfolio_optimizer/src/lib.rs
//
// Batch portfolio optimizer.
//
// Subscribes to: Event::Signal(TradeSignal), Event::Portfolio(PortfolioUpdate)
// Publishes:     Event::OptimizedSignal(OptimizedSignal)

pub mod allocator;
pub mod config;
pub mod engine;
pub mod error;
pub mod state;

pub use config::AllocationConfig;
pub use engine::PortfolioOptimizer;
pub use error::OptimizerError;
pub use state::{new_shared_state, OptimizerState, SharedOptimizerState};
