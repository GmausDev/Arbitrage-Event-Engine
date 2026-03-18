// GET /api/signals/recent?limit=N

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::state::{AppState, ShockRecord, SignalRecord};

#[derive(Deserialize)]
pub struct LimitQuery {
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize { 100 }

#[derive(Serialize)]
pub struct SignalsResponse {
    pub signals: Vec<SignalRecord>,
    pub total:   usize,
}

#[derive(Serialize)]
pub struct ShocksResponse {
    pub shocks: Vec<ShockRecord>,
    pub total:  usize,
}

pub async fn signals_handler(
    State(state): State<Arc<AppState>>,
    Query(q):     Query<LimitQuery>,
) -> Json<SignalsResponse> {
    let buf = state.signals.read().await;
    let total = buf.len();
    let signals: Vec<SignalRecord> = buf
        .iter()
        .rev()
        .take(q.limit)
        .cloned()
        .collect();
    Json(SignalsResponse { signals, total })
}

pub async fn shocks_handler(
    State(state): State<Arc<AppState>>,
    Query(q):     Query<LimitQuery>,
) -> Json<ShocksResponse> {
    let buf = state.shocks.read().await;
    let total = buf.len();
    let shocks: Vec<ShockRecord> = buf
        .iter()
        .rev()
        .take(q.limit)
        .cloned()
        .collect();
    Json(ShocksResponse { shocks, total })
}
