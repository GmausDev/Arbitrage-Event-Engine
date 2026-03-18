// GET /api/system/health

use std::sync::Arc;

use axum::{extract::State, Json};
use chrono::Utc;
use serde::Serialize;

use crate::state::AppState;

#[derive(Serialize)]
pub struct HealthResponse {
    pub status:        &'static str,
    pub uptime_secs:   u64,
    pub timestamp:     String,
    pub event_count:   u64,
    pub market_count:  usize,
    pub paused:        bool,
    pub signal_buffer: usize,
    pub exec_buffer:   usize,
}

pub async fn health_handler(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status:        "ok",
        uptime_secs:   state.uptime_secs(),
        timestamp:     Utc::now().to_rfc3339(),
        event_count:   state.total_events(),
        market_count:  state.markets.read().await.len(),
        paused:        state.is_paused(),
        signal_buffer: state.signals.read().await.len(),
        exec_buffer:   state.executions.read().await.len(),
    })
}
