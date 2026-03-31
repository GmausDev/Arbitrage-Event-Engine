// crates/persistence/src/lib.rs
//
// Persistence layer — SQLite-backed storage for trades, positions, equity
// snapshots, and audit trail events.
//
// All state in the Arbitrage Event Engine was previously ephemeral (in-memory).
// This crate provides durable storage so that:
//   - Trade history survives restarts
//   - P&L attribution is queryable (slippage, fees, gross edge per trade)
//   - Equity curves are stored for post-session analysis
//   - An audit trail records every significant system action
//
// The crate also provides an `EventRecorder` that subscribes to the event bus
// and automatically persists execution results, portfolio snapshots, and
// rejected trades.

pub mod db;
pub mod models;
pub mod recorder;
pub mod reconciliation;

pub use db::Database;
pub use models::{TradeRecord, EquitySnapshot, AuditEntry};
pub use recorder::EventRecorder;
pub use reconciliation::AccountReconciler;
