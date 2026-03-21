// crates/market_graph/src/lib.rs
// Market Graph Engine — public API surface and async event-bus integration.
//
// ┌─────────────────────────────────────────────────────────────────────────┐
// │  Crate layout                                                           │
// │                                                                         │
// │  types.rs   — MarketNode, MarketEdge, EdgeType, UpdateSource, …        │
// │  error.rs   — GraphError                                                │
// │  engine.rs  — MarketGraphEngine (synchronous core + unit tests)        │
// │  lib.rs     — re-exports + async MarketGraph wrapper (event bus)       │
// └─────────────────────────────────────────────────────────────────────────┘

pub mod config;
pub mod engine;
pub mod error;
pub mod types;

// ── Flat re-exports for convenience ──────────────────────────────────────────
pub use config::GraphConfig;
pub use engine::MarketGraphEngine;
pub use error::GraphError;
pub use types::{
    EdgeType, GraphSnapshot, MarketEdge, MarketNode, NodeChange, PropagationConfig,
    PropagationResult, UpdateSource,
};

// ── Async event-bus wrapper ───────────────────────────────────────────────────

use std::sync::Arc;

use common::{EdgeDirection, Event, EventBus, GraphUpdate, NodeProbUpdate};
use metrics::counter;
use tokio::sync::{broadcast, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Thread-safe handle to the [`MarketGraphEngine`].
///
/// Cheap to clone — all clones share the same underlying engine behind an
/// `Arc<RwLock<…>>`.
pub type SharedEngine = Arc<RwLock<MarketGraphEngine>>;

/// Centralised translation from the wire-format [`common::MarketNode`] to the
/// rich engine [`MarketNode`].
///
/// All field mapping lives here so future wire-format additions only need one
/// update rather than being scattered across the event loop.
fn wire_to_engine_node(wire: common::MarketNode) -> MarketNode {
    MarketNode {
        market_id:       wire.id.clone(),
        title:           wire.id,   // wire format has no title field
        current_prob:    wire.probability,
        volume:          Some(wire.liquidity),
        resolution_date: None,
        our_position:    0.0,
        implied_prob:    wire.probability,
        last_source:     UpdateSource::Api,
        last_update:     wire.last_update,
        tags:            Vec::new(),
    }
}

/// Async wrapper that connects a [`MarketGraphEngine`] to the system Event Bus.
///
/// # Event subscriptions
/// | Subscribes to          | Action                                      |
/// |------------------------|---------------------------------------------|
/// | `Event::Market(…)`     | `update_and_propagate` on the inner engine  |
///
/// # Events published
/// | Event                   | When                                        |
/// |-------------------------|---------------------------------------------|
/// | `Event::Graph(…)`       | After every successful propagation pass     |
pub struct MarketGraph {
    engine: SharedEngine,
    bus: EventBus,
}

impl MarketGraph {
    /// Create a new wrapper around a fresh engine with default propagation config.
    pub fn new(bus: EventBus) -> Self {
        Self {
            engine: Arc::new(RwLock::new(MarketGraphEngine::new())),
            bus,
        }
    }

    /// Create a new wrapper with propagation settings loaded from a [`GraphConfig`].
    pub fn from_config(cfg: GraphConfig, bus: EventBus) -> Self {
        let engine = MarketGraphEngine::with_config(cfg.into());
        Self {
            engine: Arc::new(RwLock::new(engine)),
            bus,
        }
    }

    /// Create a wrapper around an existing shared engine (e.g. pre-loaded from
    /// a snapshot).
    pub fn with_engine(engine: SharedEngine, bus: EventBus) -> Self {
        Self { engine, bus }
    }

    /// Returns a cloned handle to the inner engine for read/write access from
    /// other tasks (e.g. arb_agent reading implied probabilities).
    pub fn engine_handle(&self) -> SharedEngine {
        Arc::clone(&self.engine)
    }

    /// Main event loop.
    ///
    /// Subscribes to `Event::Market` on the bus and applies each update to the
    /// inner engine, then publishes a `Event::Graph` with the list of markets
    /// whose `implied_prob` changed.
    ///
    /// Runs until `cancel` is cancelled or the bus is closed.
    pub async fn run(self, cancel: CancellationToken) {
        let mut rx: broadcast::Receiver<Arc<Event>> = self.bus.subscribe();
        info!("market_graph: started — listening for MarketUpdate events");

        loop {
            let event = tokio::select! {
                _ = cancel.cancelled() => {
                    info!("market_graph: shutdown requested, exiting");
                    break;
                }
                result = rx.recv() => result,
            };

            match event {
                Ok(ev) => match ev.as_ref() {
                    Event::Market(update) => {
                        counter!("market_graph_market_updates_total").increment(1);
                        let market_id = update.market.id.clone();
                        let new_prob   = update.market.probability;

                        // ── Perform all engine mutations inside a tightly scoped
                        //    write lock.  The lock is dropped before we publish the
                        //    GraphUpdate so readers are never blocked during I/O.
                        let maybe_changes: Option<Vec<NodeChange>> = {
                            let mut engine = self.engine.write().await;

                            let is_new = engine.get_market(&market_id).is_err();

                            if is_new {
                                // First sighting — add the node.  No upstream edges exist
                                // yet, so implied_prob = current_prob and deviation = 0.
                                // No GraphUpdate is published for brand-new markets.
                                let node = wire_to_engine_node(update.market.clone());
                                engine.add_market(node);
                                None
                            } else {
                                // Sync volume/timestamp before propagating so consumers
                                // reading after the GraphUpdate see fresh metadata.
                                if let Ok(node) = engine.get_market_mut(&market_id) {
                                    node.volume = Some(update.market.liquidity);
                                    node.last_update = update.market.last_update;
                                }

                                match engine.update_and_propagate(
                                    &market_id,
                                    new_prob,
                                    UpdateSource::Api,
                                ) {
                                    Ok(result) if result.nodes_updated > 0 => {
                                        Some(result.node_changes)
                                    }
                                    Ok(_) => None, // delta below threshold
                                    Err(e) => {
                                        warn!(
                                            "market_graph: propagation error for {market_id}: {e}"
                                        );
                                        None
                                    }
                                }
                            }
                            // write lock dropped here at end of block
                        };

                        // Publish outside the lock — readers can proceed concurrently.
                        if let Some(changes) = maybe_changes {
                            let gu = GraphUpdate {
                                source_market_id: market_id,
                                node_updates: changes
                                    .into_iter()
                                    .map(|c| NodeProbUpdate {
                                        market_id:       c.market_id,
                                        old_implied_prob: c.old_implied_prob,
                                        new_implied_prob: c.new_implied_prob,
                                    })
                                    .collect(),
                            };
                            if let Err(e) = self.bus.publish(Event::Graph(gu)) {
                                warn!("market_graph: failed to publish GraphUpdate: {e}");
                            } else {
                                counter!("market_graph_updates_emitted_total").increment(1);
                            }
                        }
                    }

                    Event::RelationshipDiscovered(rd) => {
                        // Dynamically add / update a correlation edge discovered by
                        // the relationship_discovery engine.  Only add the edge when
                        // both nodes are already in the graph (they are added on first
                        // `Event::Market`); skip silently if either is absent.
                        let mut engine = self.engine.write().await;
                        let a_exists = engine.get_market(&rd.market_a).is_ok();
                        let b_exists = engine.get_market(&rd.market_b).is_ok();

                        if !a_exists || !b_exists {
                            debug!(
                                market_a = %rd.market_a,
                                market_b = %rd.market_b,
                                "market_graph: RelationshipDiscovered — one or both markets not yet in graph, skipping"
                            );
                        } else {
                            // Use signed Pearson r as edge weight; strength as confidence.
                            // For Undirected / AtoB: add a → b.
                            // For BtoA: add b → a.
                            // We do NOT add both directions to avoid circular amplification
                            // in the BFS propagation loop.
                            let (from, to) = match rd.direction {
                                EdgeDirection::BtoA => (rd.market_b.as_str(), rd.market_a.as_str()),
                                _                   => (rd.market_a.as_str(), rd.market_b.as_str()),
                            };
                            // Correlation can be negative; clamp to [-1, 1] for safety.
                            let weight = rd.correlation.clamp(-1.0, 1.0);
                            match engine.add_dependency(from, to, weight, rd.strength, EdgeType::Statistical) {
                                Ok(_) => {
                                    counter!("market_graph_relationships_applied_total").increment(1);
                                    debug!(
                                        from, to,
                                        weight, strength = rd.strength,
                                        "market_graph: added/updated statistical edge"
                                    );
                                }
                                Err(e) => {
                                    warn!("market_graph: failed to add statistical edge {from}→{to}: {e}");
                                }
                            }
                        }
                    }

                    _ => {
                        // Ignore events not relevant to this engine
                    }
                },

                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(
                        "market_graph: lagged by {n} events — consider increasing bus capacity"
                    );
                }

                Err(broadcast::error::RecvError::Closed) => {
                    info!("market_graph: event bus closed, shutting down");
                    break;
                }
            }
        }
    }
}
