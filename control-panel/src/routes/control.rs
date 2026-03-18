// POST /api/control/pause_trading
// POST /api/control/resume_trading
// POST /api/control/update_parameter
// GET  /api/control/config

use std::sync::{atomic::Ordering, Arc};

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::{config_store::TradingConfigPatch, state::AppState};

#[derive(Serialize)]
pub struct AckResponse {
    pub ok:      bool,
    pub message: String,
}

impl AckResponse {
    fn ok(msg: impl Into<String>) -> Json<Self> {
        Json(Self { ok: true, message: msg.into() })
    }
}

// ── Pause / Resume ──────────────────────────────────────────────────────────

pub async fn pause_handler(State(state): State<Arc<AppState>>) -> Json<AckResponse> {
    state.paused.store(true, Ordering::Relaxed);
    info!("control_panel: trading PAUSED");
    AckResponse::ok("trading paused")
}

pub async fn resume_handler(State(state): State<Arc<AppState>>) -> Json<AckResponse> {
    state.paused.store(false, Ordering::Relaxed);
    info!("control_panel: trading RESUMED");
    AckResponse::ok("trading resumed")
}

// ── Config ──────────────────────────────────────────────────────────────────

pub async fn get_config_handler(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let cfg = state.config.get().await;
    Json(serde_json::to_value(cfg).unwrap_or_default())
}

#[derive(Deserialize)]
pub struct UpdateParameterRequest {
    #[serde(flatten)]
    pub patch: TradingConfigPatch,
}

pub async fn update_parameter_handler(
    State(state): State<Arc<AppState>>,
    Json(req):    Json<UpdateParameterRequest>,
) -> Json<AckResponse> {
    state.config.patch(req.patch).await;
    info!("control_panel: configuration updated");
    AckResponse::ok("parameter updated")
}

// ── Execution log ───────────────────────────────────────────────────────────

use crate::state::ExecutionRecord;

#[derive(Serialize)]
pub struct ExecutionsResponse {
    pub executions: Vec<ExecutionRecord>,
    pub total:      usize,
}

pub async fn executions_handler(State(state): State<Arc<AppState>>) -> Json<ExecutionsResponse> {
    let buf = state.executions.read().await;
    let total = buf.len();
    let executions: Vec<ExecutionRecord> = buf.iter().rev().cloned().collect();
    Json(ExecutionsResponse { executions, total })
}
