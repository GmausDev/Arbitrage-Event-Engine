// control-panel/src/ws.rs
//
// WebSocket endpoint: GET /ws/stream
//
// On upgrade:
//  1. Subscribe to the broadcast channel.
//  2. Send an immediate Heartbeat so the client sees live system state.
//  3. Forward every subsequent JSON message to the client.
//  4. Respond to client ping frames; close on any error or client disconnect.

use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
};
use futures::{SinkExt, StreamExt};
use tokio::time::{interval, Duration};
use tracing::{debug, warn};

use crate::state::{AppState, WsEvent};

/// Heartbeat period.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

/// Axum handler — upgrades the HTTP connection to WebSocket.
pub async fn ws_handler(
    ws:              WebSocketUpgrade,
    State(state):    State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let (mut sender, mut receiver) = socket.split();

    // Subscribe to the broadcast bus *before* sending the initial heartbeat
    // to avoid a race where events arrive between subscribe and send.
    let mut bus_rx = state.ws_tx.subscribe();

    // ── Initial heartbeat ─────────────────────────────────────────────────
    let hb = WsEvent::Heartbeat {
        uptime_secs:  state.uptime_secs(),
        event_count:  state.total_events(),
        paused:       state.is_paused(),
        market_count: state.markets.read().await.len(),
    };
    if let Ok(json) = serde_json::to_string(&hb) {
        if sender.send(Message::Text(json)).await.is_err() {
            return; // client already gone
        }
    }

    // ── Periodic heartbeat ticker ─────────────────────────────────────────
    let mut hb_tick = interval(HEARTBEAT_INTERVAL);
    hb_tick.tick().await; // consume the immediate first tick

    loop {
        tokio::select! {
            biased;

            // Periodic heartbeat
            _ = hb_tick.tick() => {
                let market_count = state.markets.read().await.len();
                let hb = WsEvent::Heartbeat {
                    uptime_secs:  state.uptime_secs(),
                    event_count:  state.total_events(),
                    paused:       state.is_paused(),
                    market_count,
                };
                if let Ok(json) = serde_json::to_string(&hb) {
                    if sender.send(Message::Text(json)).await.is_err() {
                        debug!("control_panel: ws client disconnected (heartbeat)");
                        break;
                    }
                }
            }

            // Event bus message
            msg = bus_rx.recv() => {
                match msg {
                    Ok(json) => {
                        if sender.send(Message::Text(json)).await.is_err() {
                            debug!("control_panel: ws client disconnected (send)");
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!(n, "control_panel: ws stream lagged — some events skipped");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }

            // Inbound client frame (ping / close)
            client_msg = receiver.next() => {
                match client_msg {
                    Some(Ok(Message::Ping(data))) => {
                        let _ = sender.send(Message::Pong(data)).await;
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        debug!("control_panel: ws client closed connection");
                        break;
                    }
                    Some(Err(_)) => break,
                    _ => {} // text/binary frames from client are ignored
                }
            }
        }
    }
}
