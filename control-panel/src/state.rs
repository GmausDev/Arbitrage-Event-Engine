// control-panel/src/state.rs
//
// Shared application state for the control panel.
// All ring buffers are protected by RwLock; reads never block writers for more
// than a single append.

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};

use crate::config_store::ConfigStore;

// ---------------------------------------------------------------------------
// Ring-buffer capacities
// ---------------------------------------------------------------------------

pub const SIGNAL_RING: usize   = 500;
pub const EXEC_RING: usize     = 200;
pub const SHOCK_RING: usize    = 100;
pub const STRAT_RING: usize    = 50;
pub const EQUITY_RING: usize   = 1_000; // equity curve history points
pub const WS_BUS_CAP: usize    = 2_048;

// ---------------------------------------------------------------------------
// Record types — serialised and sent to the frontend
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalRecord {
    pub timestamp:         DateTime<Utc>,
    pub agent:             String,
    pub market_id:         String,
    pub model_probability: f64,
    pub market_price:      f64,
    pub edge:              f64,
    pub direction:         String,
    pub confidence:        f64,
    pub expected_value:    f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub timestamp:          DateTime<Utc>,
    pub market_id:          String,
    pub direction:          String,
    pub approved_fraction:  f64,
    pub fill_ratio:         f64,
    pub executed_quantity:  f64,
    pub avg_price:          f64,
    pub slippage:           f64,
    pub filled:             bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketRecord {
    pub market_id:   String,
    pub probability: f64,
    pub liquidity:   f64,
    pub bid:         f64,
    pub ask:         f64,
    pub volume:      f64,
    pub last_update: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionRecord {
    pub market_id:         String,
    pub direction:         String,
    pub size:              f64,
    pub entry_probability: f64,
    pub opened_at:         DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioSnapshot {
    pub equity:           f64,
    pub pnl:              f64,
    pub realized_pnl:     f64,
    pub unrealized_pnl:   f64,
    pub exposure:         f64,
    pub positions:        Vec<PositionRecord>,
    pub sharpe:           f64,
    pub max_drawdown:     f64,
    pub current_drawdown: f64,
    pub win_rate:         f64,
    pub position_count:   usize,
    pub last_update:      DateTime<Utc>,
}

impl Default for PortfolioSnapshot {
    fn default() -> Self {
        Self {
            equity:           10_000.0,
            pnl:              0.0,
            realized_pnl:     0.0,
            unrealized_pnl:   0.0,
            exposure:         0.0,
            positions:        vec![],
            sharpe:           0.0,
            max_drawdown:     0.0,
            current_drawdown: 0.0,
            win_rate:         0.0,
            position_count:   0,
            last_update:      Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquityPoint {
    pub timestamp: DateTime<Utc>,
    pub equity:    f64,
    pub pnl:       f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShockRecord {
    pub timestamp: DateTime<Utc>,
    pub market_id: String,
    pub magnitude: f64,
    pub direction: String,
    pub source:    String,
    pub z_score:   f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyRecord {
    pub strategy_id:  String,
    pub sharpe:       f64,
    pub max_drawdown: f64,
    pub win_rate:     f64,
    pub trade_count:  usize,
    pub promoted_at:  DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// WebSocket event envelope — tagged union serialised as { "type": "...", "data": {...} }
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum WsEvent {
    Signal(SignalRecord),
    Execution(ExecutionRecord),
    Portfolio(PortfolioSnapshot),
    Shock(ShockRecord),
    MarketUpdate {
        market_id:   String,
        probability: f64,
        liquidity:   f64,
        timestamp:   DateTime<Utc>,
    },
    StrategyDiscovered(StrategyRecord),
    MetaSignal {
        market_id:     String,
        direction:     String,
        confidence:    f64,
        expected_edge: f64,
        timestamp:     DateTime<Utc>,
    },
    /// Periodic heartbeat so clients can detect stale connections.
    Heartbeat {
        uptime_secs:  u64,
        event_count:  u64,
        paused:       bool,
        market_count: usize,
    },
}

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

pub struct AppState {
    // ── Ring buffers ──────────────────────────────────────────────────────
    pub signals:       Arc<RwLock<VecDeque<SignalRecord>>>,
    pub executions:    Arc<RwLock<VecDeque<ExecutionRecord>>>,
    pub shocks:        Arc<RwLock<VecDeque<ShockRecord>>>,
    pub strategies:    Arc<RwLock<VecDeque<StrategyRecord>>>,

    // ── Current state ────────────────────────────────────────────────────
    pub markets:       Arc<RwLock<HashMap<String, MarketRecord>>>,
    pub portfolio:     Arc<RwLock<PortfolioSnapshot>>,
    pub equity_curve:  Arc<RwLock<VecDeque<EquityPoint>>>,

    // ── Risk tracking (global high-water marks, matching performance_analytics) ─
    pub peak_equity:       Arc<RwLock<f64>>,
    pub max_drawdown_hwm:  Arc<RwLock<f64>>,

    // ── Bankroll ─────────────────────────────────────────────────────────
    pub initial_bankroll: f64,

    // ── Configuration ────────────────────────────────────────────────────
    pub config:        Arc<ConfigStore>,

    // ── Control flags ────────────────────────────────────────────────────
    pub paused:        Arc<AtomicBool>,

    // ── Counters ─────────────────────────────────────────────────────────
    pub event_count:   Arc<AtomicU64>,

    // ── WebSocket broadcast ──────────────────────────────────────────────
    /// JSON-encoded `WsEvent` messages — subscribe per connection.
    pub ws_tx:         broadcast::Sender<String>,

    // ── Lifecycle ────────────────────────────────────────────────────────
    pub started_at:    Instant,
}

impl AppState {
    /// Create a new `AppState`.  Returns the state and the WS sender (same
    /// object as `state.ws_tx` — kept for ergonomics when passing to tasks).
    ///
    /// `initial_bankroll` must match `PortfolioConfig::initial_bankroll` so
    /// that equity and drawdown figures align with Prometheus gauges.
    pub fn new(initial_bankroll: f64) -> (Self, broadcast::Sender<String>) {
        let (ws_tx, _) = broadcast::channel(WS_BUS_CAP);
        let state = Self {
            signals:      Arc::new(RwLock::new(VecDeque::with_capacity(SIGNAL_RING))),
            executions:   Arc::new(RwLock::new(VecDeque::with_capacity(EXEC_RING))),
            shocks:       Arc::new(RwLock::new(VecDeque::with_capacity(SHOCK_RING))),
            strategies:   Arc::new(RwLock::new(VecDeque::with_capacity(STRAT_RING))),
            markets:      Arc::new(RwLock::new(HashMap::new())),
            portfolio:    Arc::new(RwLock::new(PortfolioSnapshot::default())),
            equity_curve: Arc::new(RwLock::new(VecDeque::with_capacity(EQUITY_RING))),
            peak_equity:      Arc::new(RwLock::new(f64::NEG_INFINITY)),
            max_drawdown_hwm: Arc::new(RwLock::new(0.0)),
            initial_bankroll,
            config:       Arc::new(ConfigStore::default()),
            paused:       Arc::new(AtomicBool::new(false)),
            event_count:  Arc::new(AtomicU64::new(0)),
            ws_tx:        ws_tx.clone(),
            started_at:   Instant::now(),
        };
        (state, ws_tx)
    }

    // ── Convenience helpers ───────────────────────────────────────────────

    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn total_events(&self) -> u64 {
        self.event_count.load(Ordering::Relaxed)
    }

    /// Broadcast a WsEvent; silently ignores send errors (no receivers).
    pub fn broadcast(&self, ev: WsEvent) {
        if let Ok(json) = serde_json::to_string(&ev) {
            let _ = self.ws_tx.send(json);
        }
    }
}

// ---------------------------------------------------------------------------
// Ring-buffer helpers
// ---------------------------------------------------------------------------

/// Push `item` into `deque`, evicting the oldest entry when at `cap`.
pub fn ring_push<T>(deque: &mut VecDeque<T>, item: T, cap: usize) {
    if deque.len() >= cap {
        deque.pop_front();
    }
    deque.push_back(item);
}
