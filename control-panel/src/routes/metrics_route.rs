// GET /api/metrics
//
// Proxies the Prometheus text output from the system's metrics exporter
// (default: http://localhost:9000/metrics).
//
// If the exporter isn't reachable, returns a 503 with an error message.

use axum::{
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};

/// Prometheus metrics exporter address.
const PROM_ADDR: &str = "http://127.0.0.1:9000/metrics";

pub async fn metrics_handler() -> Response {
    match reqwest::get(PROM_ADDR).await {
        Ok(resp) => {
            match resp.text().await {
                Ok(body) => (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
                    body,
                )
                    .into_response(),
                Err(e) => (
                    StatusCode::BAD_GATEWAY,
                    format!("failed to read metrics body: {e}"),
                )
                    .into_response(),
            }
        }
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("metrics exporter unreachable ({PROM_ADDR}): {e}"),
        )
            .into_response(),
    }
}
