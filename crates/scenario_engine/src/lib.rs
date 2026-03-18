// crates/scenario_engine/src/lib.rs

pub mod analysis;
pub mod config;
pub mod engine;
pub mod error;
pub mod sampler;
pub mod types;

pub use config::ScenarioEngineConfig;
pub use engine::{ScenarioEngine, SharedScenarioState};
pub use types::{
    DependencyEdge, PendingSignal, Scenario, ScenarioBatch, ScenarioEngineState,
    ScenarioExpectation,
};
