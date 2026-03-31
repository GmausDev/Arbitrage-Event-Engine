// control-panel/src/auth.rs
//
// Bearer-token authentication middleware for the control panel.
//
// The token is read from the `CONTROL_PANEL_API_KEY` environment variable.
// When the variable is **not set**, auth can only be disabled by explicitly
// setting `CONTROL_PANEL_AUTH_DISABLED=true`.  If neither is set, all
// authenticated requests are rejected (fail-closed).
//
// Clients must send `Authorization: Bearer <token>` on every request
// (including the WebSocket upgrade).

use axum::{
    extract::Request,
    http::{header, StatusCode},
    middleware::Next,
    response::Response,
};

/// Auth configuration resolved once at startup from environment variables.
#[derive(Debug)]
enum AuthMode {
    /// A bearer token is required on every request.
    Token(String),
    /// Auth explicitly disabled via `CONTROL_PANEL_AUTH_DISABLED=true`.
    Disabled,
    /// Neither token nor explicit disable flag — reject everything.
    Unconfigured,
}

fn auth_mode() -> &'static AuthMode {
    use std::sync::OnceLock;
    static MODE: OnceLock<AuthMode> = OnceLock::new();
    MODE.get_or_init(|| {
        if let Ok(token) = std::env::var("CONTROL_PANEL_API_KEY") {
            if !token.is_empty() {
                return AuthMode::Token(token);
            }
        }
        if std::env::var("CONTROL_PANEL_AUTH_DISABLED")
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
        {
            tracing::warn!(
                "CONTROL_PANEL_AUTH_DISABLED=true — authentication is disabled. \
                 Do NOT use this in production."
            );
            return AuthMode::Disabled;
        }
        tracing::error!(
            "CONTROL_PANEL_API_KEY is not set and CONTROL_PANEL_AUTH_DISABLED is not true. \
             All authenticated requests will be rejected. Set CONTROL_PANEL_API_KEY for \
             production or CONTROL_PANEL_AUTH_DISABLED=true for local development."
        );
        AuthMode::Unconfigured
    })
}

/// Constant-time string comparison to prevent timing attacks on token validation.
fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// Axum middleware that rejects requests without a valid Bearer token.
pub async fn require_auth(req: Request, next: Next) -> Result<Response, StatusCode> {
    let expected = match auth_mode() {
        AuthMode::Disabled => return Ok(next.run(req).await),
        AuthMode::Unconfigured => return Err(StatusCode::SERVICE_UNAVAILABLE),
        AuthMode::Token(t) => t.as_str(),
    };

    let auth_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    match auth_header {
        Some(value) if value.starts_with("Bearer ") => {
            let provided = &value["Bearer ".len()..];
            if constant_time_eq(provided, expected) {
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
                if constant_time_eq(provided, expected) {
                    return Ok(next.run(req).await);
                }
            }
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}
