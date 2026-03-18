// crates/simulation_engine/src/replay.rs
//
// Historical market data replayer.
//
// Loads NDJSON files (one tick record per line) from a directory and makes
// them available for sequential replay via `next_tick()`.
//
// # File format
//
// Each line in a `.ndjson` (or `.json`) file must be a JSON object matching:
//
// ```json
// {"timestamp": 1710000000000, "updates": [{"market": {...}}, ...]}
// ```
//
// * `timestamp` — millisecond UNIX timestamp.
// * `updates`   — array of `MarketUpdate` objects (one per market).
//
// Files are sorted lexicographically before loading; use
// `YYYY-MM-DD_HH-MM.ndjson` naming to guarantee chronological order.
//
// # Missing data
//
// If the directory does not exist, or contains no recognisable files, the
// replayer is empty and `is_empty()` returns `true`.  The caller receives
// a warning via `tracing::warn!` but no panic occurs.

use std::collections::VecDeque;
use std::path::Path;

use common::MarketUpdate;
use serde::Deserialize;
use tracing::{debug, warn};

use crate::types::SimulationTick;

// ---------------------------------------------------------------------------
// HistoricalReplayer
// ---------------------------------------------------------------------------

/// Sequential replayer for historical `SimulationTick`s loaded from disk.
pub struct HistoricalReplayer {
    ticks: VecDeque<SimulationTick>,
}

impl HistoricalReplayer {
    /// Load all NDJSON / JSON files from `dir`, sorted lexicographically.
    ///
    /// Returns an empty replayer (with a `warn!`) if the directory is absent
    /// or contains no parseable files.
    pub fn from_dir(dir: &Path) -> Self {
        let ticks = load_dir(dir).unwrap_or_else(|e| {
            warn!("historical_replayer: failed to load {}: {e}", dir.display());
            VecDeque::new()
        });
        Self { ticks }
    }

    /// Number of ticks remaining to be replayed.
    pub fn remaining(&self) -> usize {
        self.ticks.len()
    }

    /// `true` when all ticks have been consumed.
    pub fn is_empty(&self) -> bool {
        self.ticks.is_empty()
    }

    /// Return the next tick in chronological order, or `None` if exhausted.
    pub fn next_tick(&mut self) -> Option<SimulationTick> {
        self.ticks.pop_front()
    }
}

// ---------------------------------------------------------------------------
// Private loader
// ---------------------------------------------------------------------------

/// On-disk line record: one entry per NDJSON line.
#[derive(Deserialize)]
struct TickRecord {
    pub timestamp: i64,
    pub updates:   Vec<MarketUpdate>,
}

fn load_dir(dir: &Path) -> anyhow::Result<VecDeque<SimulationTick>> {
    if !dir.exists() {
        debug!("historical_replayer: directory {} not found — empty replay", dir.display());
        return Ok(VecDeque::new());
    }

    let mut files: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|s| s.to_str())
                .map(|ext| ext == "ndjson" || ext == "json")
                .unwrap_or(false)
        })
        .collect();

    // Sort by file name for deterministic, chronological ordering.
    files.sort_by_key(|e| e.path());

    let mut all: VecDeque<SimulationTick> = VecDeque::new();
    for file in &files {
        match load_file(&file.path()) {
            Ok(ticks) => {
                debug!(
                    "historical_replayer: loaded {} ticks from {}",
                    ticks.len(),
                    file.path().display()
                );
                all.extend(ticks);
            }
            Err(e) => {
                warn!(
                    "historical_replayer: skipping {}: {e}",
                    file.path().display()
                );
            }
        }
    }
    Ok(all)
}

fn load_file(path: &Path) -> anyhow::Result<Vec<SimulationTick>> {
    let content = std::fs::read_to_string(path)?;
    let mut ticks = Vec::new();

    for (line_no, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        match serde_json::from_str::<TickRecord>(trimmed) {
            Ok(rec) => ticks.push(SimulationTick {
                timestamp:      rec.timestamp,
                market_updates: rec.updates,
            }),
            Err(e) => warn!(
                "historical_replayer: parse error at {}:{}: {e}",
                path.display(),
                line_no + 1
            ),
        }
    }
    Ok(ticks)
}
