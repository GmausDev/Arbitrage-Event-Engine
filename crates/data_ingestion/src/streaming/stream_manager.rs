// crates/data_ingestion/src/streaming/stream_manager.rs
//
// StreamManager owns multiple WebSocket connections and routes inbound messages
// to normalizers → Event Bus.
//
// In simulation mode all stream URLs are empty so no connections are opened;
// the manager waits quietly until cancellation.

use std::sync::Arc;

use metrics::counter;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    cache::snapshot_cache::SnapshotCache,
    streaming::websocket_client::{WebSocketClient, WebSocketConfig, WebSocketMessage},
};
use common::EventBus;

// ---------------------------------------------------------------------------
// StreamSpec — one registered streaming source
// ---------------------------------------------------------------------------

pub struct StreamSpec {
    pub name: String,
    pub config: WebSocketConfig,
}

// ---------------------------------------------------------------------------
// StreamManager
// ---------------------------------------------------------------------------

pub struct StreamManager {
    streams: Vec<StreamSpec>,
    bus: EventBus,
    cache: Arc<RwLock<SnapshotCache>>,
}

impl StreamManager {
    pub fn new(bus: EventBus, cache: Arc<RwLock<SnapshotCache>>) -> Self {
        Self {
            streams: vec![],
            bus,
            cache,
        }
    }

    /// Register a WebSocket stream source.
    pub fn add_stream(&mut self, spec: StreamSpec) {
        self.streams.push(spec);
    }

    /// Start all registered streams and route messages until `cancel` fires.
    pub async fn run(self, cancel: CancellationToken) {
        let mut handles = Vec::with_capacity(self.streams.len());

        for spec in self.streams {
            let client = WebSocketClient::new(spec.config);
            let cancel = cancel.clone();
            let bus = self.bus.clone();
            let cache = self.cache.clone();
            let name = spec.name.clone();

            match client.connect(cancel.clone()) {
                None => {
                    info!(stream = name, "stream_manager: streaming disabled (no URL)");
                }
                Some(mut rx) => {
                    handles.push(tokio::spawn(async move {
                        info!(stream = name, "stream_manager: stream started");
                        loop {
                            tokio::select! {
                                biased;
                                _ = cancel.cancelled() => {
                                    info!(stream = name, "stream_manager: stream stopping");
                                    break;
                                }
                                msg = rx.recv() => {
                                    match msg {
                                        Some(m) => handle_message(m, &name, &bus, &cache).await,
                                        None => {
                                            warn!(stream = name, "stream_manager: channel closed");
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }));
                }
            }
        }

        // If no streams opened, just wait for cancellation.
        if handles.is_empty() {
            cancel.cancelled().await;
            return;
        }

        for h in handles {
            let _ = h.await;
        }
    }
}

// ---------------------------------------------------------------------------
// Message handler (stub)
// ---------------------------------------------------------------------------

async fn handle_message(
    msg: WebSocketMessage,
    stream_name: &str,
    _bus: &EventBus,
    _cache: &Arc<RwLock<SnapshotCache>>,
) {
    // In a live implementation, deserialize `msg.payload` into a raw API type,
    // normalize it, update the cache, and publish an Event to the bus.
    debug!(
        stream = stream_name,
        bytes = msg.payload.len(),
        "stream_manager: received message"
    );
    counter!("data_ingestion_stream_messages_total").increment(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stream_manager_no_streams_exits_on_cancel() {
        let bus = common::EventBus::new();
        let cache = Arc::new(RwLock::new(SnapshotCache::new(10, 10)));
        let cancel = CancellationToken::new();
        let mgr = StreamManager::new(bus, cache);

        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(async move { mgr.run(cancel_clone).await });

        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
        cancel.cancel();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn stream_manager_disabled_stream_does_not_panic() {
        let bus = common::EventBus::new();
        let cache = Arc::new(RwLock::new(SnapshotCache::new(10, 10)));
        let cancel = CancellationToken::new();

        let mut mgr = StreamManager::new(bus, cache);
        mgr.add_stream(StreamSpec {
            name: "test_stream".into(),
            config: WebSocketConfig::default(), // empty URL → disabled
        });

        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(async move { mgr.run(cancel_clone).await });

        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
        cancel.cancel();
        handle.await.unwrap();
    }
}
