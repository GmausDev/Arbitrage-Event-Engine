// crates/relationship_discovery/src/engine.rs
//
// RelationshipDiscoveryEngine — async event loop.
//
// ## Pipeline position
//
// ```text
// market_scanner  ──→ Event::Market ──→ RelationshipDiscoveryEngine   ← this module
//
// RelationshipDiscoveryEngine ──→ Event::RelationshipDiscovered
//                                      └─→ market_graph (future consumer)
// ```
//
// ## Lock discipline
//
// The engine holds the write lock for the full duration of per-market
// processing (history update + all-peer pairwise computation + edge collection).
// The lock is released before any bus publish.
//
// Per-event cost: O(N × W) where N = tracked markets, W = max_history_len.
// For N=1000, W=200 this is ~200K f64 ops ≈ <1 ms — well within budget.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use common::{Event, EventBus, MarketUpdate, RelationshipDiscovered};
use metrics::counter;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    config::RelationshipDiscoveryConfig,
    correlator::{build_embedding, tokenize, try_relationship},
    types::{
        canonical_pair, new_shared_state, JointEntry, MarketHistory,
        SharedRelationshipDiscoveryState,
    },
};

// ---------------------------------------------------------------------------
// RelationshipDiscoveryEngine
// ---------------------------------------------------------------------------

pub struct RelationshipDiscoveryEngine {
    pub config: RelationshipDiscoveryConfig,
    /// Shared state — `pub` so tests and health checks can inspect it.
    pub state: SharedRelationshipDiscoveryState,
    bus: EventBus,
    rx: Option<broadcast::Receiver<Arc<Event>>>,
}

impl RelationshipDiscoveryEngine {
    pub fn new(config: RelationshipDiscoveryConfig, bus: EventBus) -> Self {
        if let Err(e) = config.validate() {
            panic!("invalid RelationshipDiscoveryConfig: {e}");
        }
        let rx = bus.subscribe();
        Self {
            config,
            state: new_shared_state(),
            bus,
            rx: Some(rx),
        }
    }

    /// Clone the shared state handle (for external inspection / tests).
    pub fn state(&self) -> SharedRelationshipDiscoveryState {
        self.state.clone()
    }

    // -----------------------------------------------------------------------
    // Async event loop
    // -----------------------------------------------------------------------

    pub async fn run(mut self, cancel: CancellationToken) {
        let mut rx = self.rx.take().expect("rx already consumed");
        info!("relationship_discovery: started");

        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    info!("relationship_discovery: cancelled, shutting down");
                    break;
                }
                result = rx.recv() => {
                    match result {
                        Ok(ev) => self.handle_event(ev.as_ref()).await,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(
                                "relationship_discovery: lagged by {n} events \
                                 — consider increasing bus capacity"
                            );
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("relationship_discovery: bus closed, shutting down");
                            break;
                        }
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Event dispatch
    // -----------------------------------------------------------------------

    async fn handle_event(&self, event: &Event) {
        if let Event::Market(update) = event {
            let discovered = self.process_market_update(update).await;
            for rel in discovered {
                self.publish_relationship(rel);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Core processing — holds write lock, returns edges to publish
    // -----------------------------------------------------------------------

    async fn process_market_update(
        &self,
        update: &MarketUpdate,
    ) -> Vec<RelationshipDiscovered> {
        let now        = Utc::now();
        let new_prob   = update.market.probability;
        let market_id  = &update.market.id;

        counter!("relationship_discovery_market_updates_total").increment(1);

        // Reject non-finite probabilities before they can corrupt any state.
        if new_prob.is_nan() || new_prob.is_infinite() {
            warn!(
                market_id = %market_id,
                "relationship_discovery: ignoring non-finite probability, skipping update"
            );
            return vec![];
        }

        let mut state = self.state.write().await;
        state.events_processed += 1;

        // ── Step 1: Snapshot existing peer IDs before mutating histories ─────
        //
        // We collect the keys first so that the subsequent `.entry()` call on
        // the same HashMap does not conflict with a live iterator borrow.
        let peer_ids: Vec<String> = state
            .histories
            .keys()
            .filter(|id| *id != market_id)
            .cloned()
            .collect();

        // ── Step 2: Update this market's rolling history ─────────────────────
        {
            let h = state
                .histories
                .entry(market_id.clone())
                .or_insert_with(|| MarketHistory {
                    last_seen: now,
                    ..Default::default()
                });

            // Always record the wall-clock time of this update so that
            // future eviction logic (if added) sees the market as recently
            // active even when micro-updates are noise-filtered.
            h.last_seen = now;

            // Noise gate — skip micro-updates that carry no information.
            if let Some(&last) = h.prices.back() {
                if (new_prob - last).abs() < self.config.min_prob_change {
                    counter!("relationship_discovery_noise_filtered_total").increment(1);
                    return vec![];
                }
            }

            h.prices.push_back(new_prob);
            if h.prices.len() > self.config.max_history_len {
                h.prices.pop_front();
            }

            // Mark this market as having fresh price data available for pairing.
            h.dirty = true;

            // Cache the market's TF-IDF embedding (computed once, reused forever).
            if h.embedding.is_none() {
                h.embedding = Some(build_embedding(&tokenize(market_id)));
            }
        } // mutable borrow of state.histories released here

        // Warm-up guard: do nothing until this market has enough history.
        if state.histories[market_id].prices.len() < self.config.min_history_len {
            return vec![];
        }

        // Snapshot the updated market's embedding (clone → no live borrow).
        let emb_updated: Vec<(String, f64)> =
            state.histories[market_id].embedding.clone().unwrap_or_default();

        // ── Step 3: Process each peer ────────────────────────────────────────
        //
        // Exactly one joint entry is pushed per market-update event.
        // We pair with the first peer whose dirty flag is set (meaning
        // it too has new price data since its last pairing), then clear
        // both dirty flags and break — preventing a single update from
        // producing synthetic O(N) observations against stale peers.
        let mut signals: Vec<RelationshipDiscovered> = Vec::new();
        let mut paired = false;

        for peer_id in &peer_ids {
            // ── 3a: Read peer's most recent price (borrow ends at semicolon) ──
            //
            // Require peer.dirty so we only pair with markets that have had
            // a genuine price update since the last joint observation.
            let peer_prob_opt: Option<f64> = state
                .histories
                .get(peer_id)
                .filter(|h| h.prices.len() >= self.config.min_history_len && h.dirty)
                .and_then(|h| h.prices.back().copied());

            let peer_prob = match peer_prob_opt {
                Some(p) => p,
                None    => continue,
            };

            // ── 3b: Clone peer embedding (borrow ends at semicolon) ───────────
            let peer_emb_cached: Option<Vec<(String, f64)>> = state
                .histories
                .get(peer_id)
                .and_then(|h| h.embedding.clone());

            // Resolve or lazily compute the peer's embedding.
            let peer_emb: Vec<(String, f64)> = peer_emb_cached
                .unwrap_or_else(|| build_embedding(&tokenize(peer_id)));

            // Cache it on the peer's history if it was missing.
            if let Some(ph) = state.histories.get_mut(peer_id) {
                if ph.embedding.is_none() {
                    ph.embedding = Some(peer_emb.clone());
                }
            }
            // mutable borrow of state.histories released

            // ── 3c: Determine canonical pair ordering ─────────────────────────
            let pair = canonical_pair(market_id, peer_id);
            let (pa, pb) = if market_id.as_str() <= peer_id.as_str() {
                (new_prob, peer_prob)
            } else {
                (peer_prob, new_prob)
            };

            // ── 3d: Update joint entry, then attempt scoring ──────────────────
            //
            // We scope `joint` tightly so that the mutable borrow of
            // state.joint_entries ends before we touch state.relationships_discovered.
            let rel_opt = {
                let joint = state
                    .joint_entries
                    .entry(pair.clone())
                    .or_insert_with(JointEntry::default);

                joint.push(pa, pb, self.config.max_history_len, self.config.price_change_threshold);

                // `emb_updated` is the embedding for the updated market.
                // For the canonical pair, determine which embedding belongs to which slot.
                let (ea, eb): (&[(String, f64)], &[(String, f64)]) =
                    if market_id.as_str() <= peer_id.as_str() {
                        (&emb_updated, &peer_emb)
                    } else {
                        (&peer_emb, &emb_updated)
                    };

                try_relationship(&pair, joint, &self.config, ea, eb, now)
                // `joint` borrow released here
            };

            // ── 3e: Synchronised-pairing dirty-flag bookkeeping ──────────────
            //
            // Both markets' current prices are now captured in the joint entry.
            // Clear the peer's dirty flag so its current price isn't re-used
            // by another market before it next updates.  The updated market's
            // dirty flag is cleared below after the loop.
            if let Some(ph) = state.histories.get_mut(peer_id) {
                ph.dirty = false;
            }
            paired = true;

            if let Some(rel) = rel_opt {
                state.relationships_discovered += 1;
                signals.push(rel);
            }

            // One joint entry per market-update: stop after the first pairing.
            break;
        }

        // ── Step 4: Clear updated market's dirty flag ────────────────────────
        //
        // Only cleared when a pairing actually happened; if no dirty peer was
        // found this cycle, dirty stays true so the next peer update can pair.
        if paired {
            if let Some(h) = state.histories.get_mut(market_id) {
                h.dirty = false;
            }
        }

        // ── Step 5: Periodic eviction of stale state ─────────────────────────
        state.events_since_eviction += 1;
        if state.events_since_eviction >= self.config.eviction_interval {
            state.events_since_eviction = 0;
            evict_stale_state(&mut state, &self.config, now);
        }

        signals
        // Write lock released when `state` is dropped at end of scope.
    }

    // -----------------------------------------------------------------------
    // 7. Graph Update Publisher
    // -----------------------------------------------------------------------

    fn publish_relationship(&self, rel: RelationshipDiscovered) {
        counter!("relationship_discovery_emitted_total").increment(1);
        debug!(
            market_a  = %rel.market_a,
            market_b  = %rel.market_b,
            strength  = rel.strength,
            direction = ?rel.direction,
            corr      = rel.correlation,
            mi        = rel.mutual_information,
            emb       = rel.embedding_similarity,
            "relationship_discovery: RelationshipDiscovered emitted"
        );
        if let Err(e) = self.bus.publish(Event::RelationshipDiscovered(rel)) {
            warn!("relationship_discovery: failed to publish: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// State eviction
// ---------------------------------------------------------------------------

/// Remove markets that have not been seen within `config.market_ttl_secs` and
/// drop all `joint_entries` that reference an evicted market.
///
/// Called periodically (every `config.eviction_interval` post-warmup events)
/// while the write lock is held, so no additional synchronisation is needed.
fn evict_stale_state(
    state:  &mut crate::types::RelationshipDiscoveryState,
    config: &crate::config::RelationshipDiscoveryConfig,
    now:    DateTime<Utc>,
) {
    // Collect IDs of markets whose last update is older than the TTL.
    let stale: Vec<String> = state
        .histories
        .iter()
        .filter(|(_, h)| {
            // Use signed arithmetic so a backward clock jump is treated as
            // "not yet expired" rather than an enormous positive age.
            let age = (now - h.last_seen).num_seconds();
            age >= 0 && (age as u64) >= config.market_ttl_secs
        })
        .map(|(id, _)| id.clone())
        .collect();

    if stale.is_empty() {
        return;
    }

    for id in &stale {
        state.histories.remove(id);
    }

    // Drop every pair entry that references an evicted market.
    state
        .joint_entries
        .retain(|(a, b), _| state.histories.contains_key(a) && state.histories.contains_key(b));

    counter!("relationship_discovery_evictions_total").increment(stale.len() as u64);
    debug!(
        evicted = stale.len(),
        pairs_remaining = state.joint_entries.len(),
        "relationship_discovery: evicted stale markets"
    );
}
