// crates/world_model/src/lib.rs

pub mod config;
pub mod constraints;
pub mod engine;
pub mod error;
pub mod inference;
pub mod types;

pub use config::WorldModelConfig;
pub use engine::{SharedWorldState, WorldModelEngine};
pub use types::{
    ConstraintType, LogicalConstraint, MarketVariable, WorldDependency, WorldInferenceResult,
    WorldState,
};
