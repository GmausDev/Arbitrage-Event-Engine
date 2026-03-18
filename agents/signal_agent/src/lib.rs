// agents/signal_agent/src/lib.rs
// Signal Agent — converts Bayesian posterior beliefs into sized TradeSignal events.
//
// Subscribes to: Event::Posterior(PosteriorUpdate), Event::Market(MarketUpdate)
// Publishes:     Event::Signal(TradeSignal)

pub mod config;
pub mod engine;
pub mod math;
pub mod state;
pub mod types;

pub use config::SignalAgentConfig;
pub use engine::SignalAgent;
pub use state::{new_shared_state, MarketBelief, SharedSignalState, SignalAgentState};
pub use types::SignalAgentError;
