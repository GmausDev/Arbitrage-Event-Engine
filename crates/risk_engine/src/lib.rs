// crates/risk_engine/src/lib.rs
// Portfolio exposure tracking and position-sizing risk limits.
//
// Subscribes to: Event::Signal(TradeSignal)
// Publishes:     Event::ApprovedTrade(ApprovedTrade)

pub mod config;
pub mod engine;
pub mod error;
pub mod state;
pub mod types;

pub use config::RiskConfig;
pub use engine::RiskEngine;
pub use error::RiskEngineError;
pub use state::{new_shared_state, Position, RiskState, SharedRiskState};
pub use types::RejectionReason;
