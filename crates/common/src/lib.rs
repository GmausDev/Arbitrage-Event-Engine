// crates/common/src/lib.rs
// Shared types, events, and the Event Bus used by all crates and agents.

pub mod types;
pub mod events;
pub mod event_bus;
pub mod snapshot;

pub use types::*;
pub use events::*;
pub use event_bus::EventBus;
pub use snapshot::Snapshot;
