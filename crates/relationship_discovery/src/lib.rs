// crates/relationship_discovery/src/lib.rs
//
// Relationship Discovery AI — automatically discovers statistical and semantic
// relationships between prediction markets and emits Event::RelationshipDiscovered
// for consumption by the Market Graph Engine.

pub mod config;
pub mod types;
pub(crate) mod correlator;
mod engine;

pub use config::RelationshipDiscoveryConfig;
pub use engine::RelationshipDiscoveryEngine;
pub use types::{RelationshipDiscoveryState, SharedRelationshipDiscoveryState};
