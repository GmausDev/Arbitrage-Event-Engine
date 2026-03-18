// crates/execution_sim/src/lib.rs
//
// Simulated order execution — applies Gaussian slippage and probabilistic
// partial fills to every `Event::ApprovedTrade`, then updates portfolio state.
//
// Subscribes to: Event::ApprovedTrade
// Publishes:     Event::Execution(ExecutionResult), Event::Portfolio(PortfolioUpdate)

pub mod config;
pub mod engine;
pub mod state;
pub mod types;

pub use config::ExecutionConfig;
pub use engine::ExecutionSimulator;
pub type ExecutionSim = ExecutionSimulator;
pub use state::{new_shared_state, ExecutionState, SharedExecutionState};
pub use types::ExecutionOrder;
