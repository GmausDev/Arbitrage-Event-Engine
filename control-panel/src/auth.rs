// control-panel/src/auth.rs
//
// Bearer-token authentication middleware for the control panel.
//
// The token is read from the `CONTROL_PANEL_API_KEY` environment variable.
// When the variable is **not set**, authentication is disabled and all
// requests are allowed through (development mode).
//
// Clients must send `Authorization: Bearer <token>` on every request
// (including the WebSocket upgrade).

use axum::{
    extract::Request,
    http::{header, StatusCode},
    middleware::Next,
    response::Response,
};

/// Cached expected token (read once from env at first call).
fn expected_token() -> Option<&'static str> {
    use std::sync::OnceLock;
    static TOKEN: OnceLock<Option<String>> = OnceLock::new();
    TOKEN
        .get_or_init(|| std::env::var("CONTROL_PANEL_API_KEY").ok())
        .as_deref()
}

/// Axum middleware that rejects requests without a valid Bearer token.
pub async fn require_auth(req: Request, next: Next) -> Result<Response, StatusCode> {
    let Some(expected) = expected_token() else {
        // No token configured — allow all requests (dev mode).
        return Ok(next.run(req).await);
    };

    let auth_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    match auth_header {
        Some(value) if value.starts_with("Bearer ") => {
            let provided = &value["Bearer ".len()..];
            if provided == expected {
                Ok(next.run(req).await)
            } else {
                Err(StatusCode::UNAUTHORIZED)
            }
        }
        // Also accept token as query param `?token=...` for WebSocket clients
        // that cannot set custom headers (e.g. browser-native WebSocket API).
        _ => {
            let uri = req.uri().to_string();
            if let Some(pos) = uri.find("token=") {
                let rest = &uri[pos + 6..];
                let provided = rest.split('&').next().unwrap_or(rest);
                if provided == expected {
                    return Ok(next.run(req).await);
                }
            }
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}
