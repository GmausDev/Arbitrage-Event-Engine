// control-panel/src/lib.rs
//
// Mission Control — real-time web dashboard for the prediction market agent.
//
// ## Integration (in prediction-agent/src/main.rs)
//
// ```rust
// use control_panel::{AppState, ControlPanelServer};
// use std::net::SocketAddr;
//
// let (app_state, _) = AppState::new();
// let state = std::sync::Arc::new(app_state);
//
// // Spawn the event collector — taps the existing bus
// control_panel::event_collector::EventCollector::spawn(bus.clone(), state.clone(), cancel.clone());
//
// // Spawn the HTTP server
// let addr: SocketAddr = "0.0.0.0:3001".parse().unwrap();
// let server = ControlPanelServer::new(state, addr);
// tokio::spawn(server.run(cancel.clone()));
// ```

pub mod auth;
pub mod config_store;
pub mod event_collector;
pub mod routes;
pub mod server;
pub mod state;
pub mod ws;

pub use server::ControlPanelServer;
pub use state::AppState;
