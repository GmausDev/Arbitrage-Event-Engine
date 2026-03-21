// crates/data_ingestion/src/lib.rs
//
// Public API surface for the data_ingestion crate.

pub mod cache;
pub mod config;
pub mod connectors;
pub mod ingestion_engine;
pub mod matcher;
pub mod normalization;
pub mod scheduler;
pub mod streaming;

pub use cache::snapshot_cache::SnapshotCache;
pub use config::IngestionConfig;
pub use ingestion_engine::DataIngestionEngine;
