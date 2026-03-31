// control-panel/src/server.rs
//
// Builds and runs the Axum HTTP server.
//
// ## Endpoints
//
//  GET  /api/system/health
//  GET  /api/metrics
//  GET  /api/portfolio
//  GET  /api/portfolio/equity_curve
//  GET  /api/signals/recent
//  GET  /api/signals/shocks
//  GET  /api/markets/active
//  GET  /api/strategies
//  GET  /api/executions
//  GET  /api/control/config
//  POST /api/control/pause_trading
//  POST /api/control/resume_trading
//  POST /api/control/update_parameter
//  GET  /ws/stream

use std::{net::SocketAddr, sync::Arc};

use axum::{
    middleware,
    routing::{get, post},
    Router,
};
use tower_http::cors::{AllowOrigin, CorsLayer};
use axum::http::{header, Method};
use tracing::info;
use tokio_util::sync::CancellationToken;

use crate::{
    auth::require_auth,
    routes::{
        control::{executions_handler, get_config_handler, pause_handler, resume_handler,
                   update_parameter_handler},
        health::health_handler,
        markets::{markets_handler, strategies_handler},
        metrics_route::metrics_handler,
        portfolio::{equity_curve_handler, portfolio_handler},
        signals::{shocks_handler, signals_handler},
    },
    state::AppState,
    ws::ws_handler,
};

pub struct ControlPanelServer {
    state: Arc<AppState>,
    addr:  SocketAddr,
}

impl ControlPanelServer {
    pub fn new(state: Arc<AppState>, addr: SocketAddr) -> Self {
        Self { state, addr }
    }

    pub async fn run(self, cancel: CancellationToken) -> anyhow::Result<()> {
        // Restrict CORS to trusted origins.  When CONTROL_PANEL_CORS_ORIGIN
        // is set (e.g. "http://localhost:3000"), only that origin is allowed.
        // Otherwise we fall back to permissive mode for local development.
        let cors = match std::env::var("CONTROL_PANEL_CORS_ORIGIN") {
            Ok(origin) => CorsLayer::new()
                .allow_origin(AllowOrigin::exact(origin.parse().unwrap_or_else(|_| {
                    tracing::warn!("CONTROL_PANEL_CORS_ORIGIN is not a valid header value, falling back to localhost");
                    "http://localhost:3000".parse().unwrap()
                })))
                .allow_methods([Method::GET, Method::POST])
                .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]),
            Err(_) => {
                tracing::warn!("CONTROL_PANEL_CORS_ORIGIN not set — CORS is permissive (dev mode)");
                CorsLayer::permissive()
            }
        };

        let app = Router::new()
            // System — health is public (no auth needed for load-balancer probes)
            .route("/api/system/health",            get(health_handler))
            .route("/api/metrics",                  get(metrics_handler))
            // All other routes require authentication
            .route("/api/portfolio",                get(portfolio_handler))
            .route("/api/portfolio/equity_curve",   get(equity_curve_handler))
            .route("/api/signals/recent",           get(signals_handler))
            .route("/api/signals/shocks",           get(shocks_handler))
            .route("/api/markets/active",           get(markets_handler))
            .route("/api/strategies",               get(strategies_handler))
            .route("/api/executions",               get(executions_handler))
            .route("/api/control/config",           get(get_config_handler))
            .route("/api/control/pause_trading",    post(pause_handler))
            .route("/api/control/resume_trading",   post(resume_handler))
            .route("/api/control/update_parameter", post(update_parameter_handler))
            .route("/ws/stream",                    get(ws_handler))
            // Auth middleware — applied to all routes; skips when
            // CONTROL_PANEL_API_KEY is not set (dev mode).
            .layer(middleware::from_fn(require_auth))
            // CORS
            .layer(cors)
            .with_state(self.state);

        info!("control_panel: listening on http://{}", self.addr);

        let listener = tokio::net::TcpListener::bind(self.addr).await?;
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { cancel.cancelled().await })
            .await?;

        info!("control_panel: server shut down");
        Ok(())
    }
}
