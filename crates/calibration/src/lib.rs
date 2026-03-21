// crates/calibration/src/lib.rs
//
// Model calibration tracker.
//
// Subscribes to Event::Posterior to record predictions, then scores them
// against resolved outcomes from Event::Market.  Emits Event::CalibrationUpdate
// so bayesian_engine can adjust its market-precision parameter over time.

pub mod config;
pub mod engine;
pub mod state;
pub mod types;

pub use config::CalibrationConfig;
pub use engine::CalibrationEngine;
