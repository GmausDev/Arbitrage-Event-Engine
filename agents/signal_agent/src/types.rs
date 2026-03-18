// agents/signal_agent/src/types.rs
// Error types for the Signal Agent.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SignalAgentError {
    #[error("event bus closed")]
    BusClosed,
}
