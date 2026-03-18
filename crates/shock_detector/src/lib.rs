// crates/shock_detector/src/lib.rs
//
// Information Shock Detector — detects sudden or unusual information events
// that could impact market probabilities, fusing price and sentiment signals.

pub mod config;
pub mod detector;
pub mod engine;
pub mod types;

pub use config::ShockDetectorConfig;
pub use engine::{InformationShockDetector, SharedShockState};
pub use types::ShockDetectorState;
