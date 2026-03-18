// crates/simulation_engine/src/lib.rs
//
// Simulation Engine — backtesting, Monte Carlo simulation, and live
// performance observation for the Prediction Market Agent.
//
// # Role in the pipeline
//
// In production (SimulationMode::LiveObserver) the engine sits at the tail
// of the pipeline, subscribing to Event::Execution and Event::Portfolio
// to accumulate real-time performance metrics without interfering with the
// live system.
//
// In backtesting modes:
//
// * MonteCarlo      — runs internal path simulations; does not publish.
// * HistoricalReplay— publishes Event::Market from NDJSON files so the
//                     full pipeline runs on real historical data.
// * ParameterSweep  — sweeps volatility / drift parameters via MC.
//
// # Subscribes to
// Event::Execution, Event::Portfolio
//
// # Publishes
// Event::Market (HistoricalReplay mode only)

pub mod config;
pub mod engine;
pub mod generators;
pub mod metrics;
pub mod replay;
pub mod types;

pub use config::{MarketSpec, MonteCarloConfig, ParameterSweepPoint, SimulationConfig};
pub use engine::SimulationEngine;
pub use generators::MarketGenerator;
pub use metrics::SimMetricsCollector;
pub use replay::HistoricalReplayer;
pub use types::{SimulationMode, SimulationResult, SimulationTick, SweepResult};
