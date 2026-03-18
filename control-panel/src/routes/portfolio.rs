// GET /api/portfolio
// GET /api/portfolio/equity_curve

use std::sync::Arc;

use axum::{extract::State, Json};
use serde::Serialize;

use crate::state::{AppState, EquityPoint, PortfolioSnapshot};

#[derive(Serialize)]
pub struct EquityCurveResponse {
    pub points: Vec<EquityPoint>,
}

pub async fn portfolio_handler(State(state): State<Arc<AppState>>) -> Json<PortfolioSnapshot> {
    Json(state.portfolio.read().await.clone())
}

pub async fn equity_curve_handler(
    State(state): State<Arc<AppState>>,
) -> Json<EquityCurveResponse> {
    let points = state.equity_curve.read().await.iter().cloned().collect();
    Json(EquityCurveResponse { points })
}
