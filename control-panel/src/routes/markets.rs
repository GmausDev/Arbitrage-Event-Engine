// GET /api/markets/active

use std::sync::Arc;

use axum::{extract::State, Json};
use serde::Serialize;

use crate::state::{AppState, MarketRecord, StrategyRecord};

#[derive(Serialize)]
pub struct MarketsResponse {
    pub markets: Vec<MarketRecord>,
    pub count:   usize,
}

#[derive(Serialize)]
pub struct StrategiesResponse {
    pub strategies: Vec<StrategyRecord>,
    pub count:      usize,
}

pub async fn markets_handler(State(state): State<Arc<AppState>>) -> Json<MarketsResponse> {
    let map = state.markets.read().await;
    let mut markets: Vec<MarketRecord> = map.values().cloned().collect();
    // Sort by last_update descending so recently-active markets appear first.
    markets.sort_by(|a, b| b.last_update.cmp(&a.last_update));
    let count = markets.len();
    Json(MarketsResponse { markets, count })
}

pub async fn strategies_handler(State(state): State<Arc<AppState>>) -> Json<StrategiesResponse> {
    let buf = state.strategies.read().await;
    let count = buf.len();
    // Most recently promoted first.
    let strategies: Vec<StrategyRecord> = buf.iter().rev().cloned().collect();
    Json(StrategiesResponse { strategies, count })
}
