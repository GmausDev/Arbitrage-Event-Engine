// crates/scenario_engine/src/error.rs

use thiserror::Error;

// TODO: wire into validation in sample_scenarios and analysis functions.
#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum ScenarioEngineError {
    #[error("empty belief state: cannot generate scenarios with no markets")]
    EmptyBeliefState,

    #[error("invalid sample size {0}: must be > 0")]
    InvalidSampleSize(usize),

    #[error("invalid probability {0}: must be in [0.0, 1.0]")]
    InvalidProbability(f64),

    #[error("invalid dependency weight {0}: must be in [-1.0, 1.0]")]
    InvalidWeight(f64),
}
