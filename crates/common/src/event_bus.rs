// crates/common/src/event_bus.rs
// Thin wrapper around tokio::sync::broadcast that is the sole
// communication channel between all engines and agents.
//
// Events are wrapped in Arc before sending so the broadcast fan-out clones
// only a pointer (8 bytes) rather than the full event payload.  Subscribers
// receive Arc<Event> and dereference with `.as_ref()` or `match ev.as_ref()`.

use std::sync::Arc;

use crate::events::Event;
use tokio::sync::broadcast;

/// Default channel capacity (number of events buffered before lagging).
const DEFAULT_CAPACITY: usize = 1_024;

/// The shared Event Bus.
///
/// Clone the `EventBus` to hand copies to each engine/agent — they all
/// share the same underlying broadcast channel.
///
/// # Fan-out
/// Every subscriber (`subscribe()`) receives every event independently.
/// Slow consumers will see `RecvError::Lagged` if they fall behind;
/// they should log the lag and continue.
///
/// # Event ownership
/// Events are wrapped in `Arc` internally.  Subscribers receive `Arc<Event>`;
/// match on them with `match ev.as_ref() { Event::Market(u) => ... }`.
#[derive(Clone, Debug)]
pub struct EventBus {
    sender: broadcast::Sender<Arc<Event>>,
}

impl EventBus {
    /// Create a new bus with the default channel capacity.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    /// Create a new bus with a custom channel capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    /// Subscribe to the bus. Each subscriber receives every event as `Arc<Event>`.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Event>> {
        self.sender.subscribe()
    }

    /// Publish an event to all current subscribers.
    ///
    /// The event is heap-allocated once and shared via `Arc`; each subscriber
    /// clone is a pointer copy.
    ///
    /// Returns the number of receivers the event was sent to, or an error
    /// if there are no active subscribers (safe to ignore during startup).
    pub fn publish(&self, event: Event) -> Result<usize, broadcast::error::SendError<Arc<Event>>> {
        self.sender.send(Arc::new(event))
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}
