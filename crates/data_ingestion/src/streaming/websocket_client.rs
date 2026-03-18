// crates/data_ingestion/src/streaming/websocket_client.rs
//
// Reconnecting WebSocket client using tokio-tungstenite.

use futures::StreamExt;
use tokio::{
    sync::mpsc,
    time::{sleep, Duration},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct WebSocketConfig {
    /// WebSocket endpoint URL (e.g. "wss://api.polymarket.com/ws/v2").
    pub url: String,
    /// Delay before the first reconnection attempt (milliseconds).
    pub reconnect_delay_ms: u64,
    /// Maximum reconnection attempts; 0 = unlimited.
    pub max_retries: u32,
    /// Inbound message channel buffer size.
    pub channel_buffer: usize,
}

impl Default for WebSocketConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            reconnect_delay_ms: 2_000,
            max_retries: 5,
            channel_buffer: 256,
        }
    }
}

// ---------------------------------------------------------------------------
// Message type
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct WebSocketMessage {
    /// Raw text payload from the server.
    pub payload: String,
    /// Source URL — useful when a `StreamManager` handles multiple connections.
    pub source_url: String,
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

pub struct WebSocketClient {
    config: WebSocketConfig,
}

impl WebSocketClient {
    pub fn new(config: WebSocketConfig) -> Self {
        Self { config }
    }

    /// Start the reconnecting WebSocket loop.
    ///
    /// Returns a receiver for inbound messages, or `None` if the URL is empty
    /// (simulation mode / no real endpoint configured).
    ///
    /// The loop runs until `cancel` fires or `max_retries` is exhausted.
    pub fn connect(
        &self,
        cancel: CancellationToken,
    ) -> Option<mpsc::Receiver<WebSocketMessage>> {
        if self.config.url.is_empty() {
            info!("websocket_client: no URL configured — streaming disabled for this connector");
            return None;
        }

        let (tx, rx) = mpsc::channel(self.config.channel_buffer);
        let cfg = self.config.clone();

        tokio::spawn(async move {
            reconnect_loop(cfg, tx, cancel).await;
        });

        Some(rx)
    }
}

// ---------------------------------------------------------------------------
// Reconnection loop (internal)
// ---------------------------------------------------------------------------

async fn reconnect_loop(
    cfg: WebSocketConfig,
    tx: mpsc::Sender<WebSocketMessage>,
    cancel: CancellationToken,
) {
    let mut attempts: u32 = 0;

    loop {
        if cancel.is_cancelled() {
            break;
        }

        if cfg.max_retries > 0 && attempts >= cfg.max_retries {
            warn!(
                url = cfg.url,
                attempts,
                "websocket_client: max retries reached, giving up"
            );
            break;
        }

        if attempts > 0 {
            // Exponential backoff capped at 60 s.
            let delay_ms = (cfg.reconnect_delay_ms * (1 << attempts.min(5))).min(60_000);
            info!(
                url = cfg.url,
                attempt = attempts,
                delay_ms,
                "websocket_client: reconnecting"
            );
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                _ = sleep(Duration::from_millis(delay_ms)) => {}
            }
        }

        attempts += 1;

        let connection_start = std::time::Instant::now();
        match connect_once(&cfg, &tx, &cancel).await {
            Ok(()) => {
                info!(url = cfg.url, "websocket_client: stream ended cleanly");
                break;
            }
            Err(e) => {
                warn!(url = cfg.url, err = %e, "websocket_client: connection error");
                // Reset the attempt counter if the connection was healthy for > 60 s
                // so that long-running connections that eventually drop get a full set
                // of retry attempts rather than giving up immediately.
                if connection_start.elapsed().as_secs() >= 60 {
                    attempts = 0;
                }
                // Loop continues — will retry after backoff.
            }
        }
    }
}

/// Attempt a single WebSocket connection using tokio-tungstenite.
async fn connect_once(
    config: &WebSocketConfig,
    tx: &mpsc::Sender<WebSocketMessage>,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    if config.url.is_empty() {
        return Err(anyhow::anyhow!("empty URL"));
    }
    let (ws_stream, _) = connect_async(&config.url)
        .await
        .map_err(|e| anyhow::anyhow!("ws connect failed: {e}"))?;

    tracing::info!(url = %config.url, "websocket: connected");

    let (_, mut read) = ws_stream.split();

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                tracing::debug!("websocket: cancelled");
                return Ok(());
            }
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if tx.send(WebSocketMessage {
                            payload: text.to_string(),
                            source_url: config.url.clone(),
                        }).await.is_err() {
                            return Err(anyhow::anyhow!("message receiver dropped"));
                        }
                    }
                    Some(Ok(Message::Binary(bytes))) => {
                        let payload = String::from_utf8_lossy(&bytes).into_owned();
                        if tx.send(WebSocketMessage {
                            payload,
                            source_url: config.url.clone(),
                        }).await.is_err() {
                            return Err(anyhow::anyhow!("message receiver dropped"));
                        }
                    }
                    Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {} // ignore
                    Some(Ok(Message::Close(_))) | None => {
                        return Err(anyhow::anyhow!("websocket closed by server"));
                    }
                    Some(Err(e)) => {
                        return Err(anyhow::anyhow!("websocket error: {e}"));
                    }
                    Some(Ok(_)) => {} // frame, close frame variants
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_url_returns_none() {
        let client = WebSocketClient::new(WebSocketConfig::default());
        let cancel = CancellationToken::new();
        assert!(client.connect(cancel).is_none());
    }

    #[tokio::test]
    async fn nonzero_url_returns_receiver() {
        let cfg = WebSocketConfig {
            url: "wss://example.com/ws".into(),
            max_retries: 1,
            reconnect_delay_ms: 10,
            channel_buffer: 4,
        };
        let cancel = CancellationToken::new();
        let result = WebSocketClient::new(cfg).connect(cancel.clone());
        assert!(result.is_some());
        cancel.cancel(); // clean up background task
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[test]
    fn default_config_has_empty_url() {
        let cfg = WebSocketConfig::default();
        assert!(cfg.url.is_empty());
    }
}
