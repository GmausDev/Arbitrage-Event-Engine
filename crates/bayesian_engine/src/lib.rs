// crates/bayesian_engine/src/lib.rs
// Bayesian Probability Engine — public API surface and async event-bus integration.
//
// ┌─────────────────────────────────────────────────────────────────────────┐
// │  Crate layout                                                           │
// │                                                                         │
// │  error.rs  — BayesianError                                              │
// │  types.rs  — BeliefState, EvidenceSource, EvidenceEntry, math helpers  │
// │  engine.rs — BayesianEngine (synchronous core + unit tests)            │
// │  lib.rs    — re-exports + async BayesianModel wrapper (event bus)      │
// └─────────────────────────────────────────────────────────────────────────┘
//
// Subscribes to: Event::Market(MarketUpdate)
//                Event::Graph(GraphUpdate)
//                Event::Sentiment(SentimentUpdate)
// Publishes:     Event::Posterior(PosteriorUpdate)

pub mod engine;
pub mod error;
pub mod types;

// ── Flat re-exports ───────────────────────────────────────────────────────────

pub use engine::BayesianEngine;
pub use error::BayesianError;
pub use types::{BeliefState, EvidenceEntry, EvidenceSource, SignalResult};

// ── Async event-bus wrapper ───────────────────────────────────────────────────

use std::sync::Arc;

use chrono::Utc;
use common::{Event, EventBus, PosteriorUpdate};
use market_graph::MarketGraphEngine;
use metrics::counter;
use tokio::sync::{broadcast, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, trace, warn};
use types::{delta_prob_to_log_odds, MAX_HISTORY};

/// Thread-safe handle to a [`BayesianEngine`].
///
/// Cheap to clone — all clones share the same underlying engine behind
/// an `Arc<RwLock<…>>`.
pub type SharedBayesianEngine = Arc<RwLock<BayesianEngine>>;

/// Thread-safe handle to a [`MarketGraphEngine`].
///
/// Passed to [`BayesianModel::with_graph`] to enable edge-weight-guided
/// downstream influence propagation in the `GraphUpdate` handler.
pub type SharedGraphEngine = Arc<RwLock<MarketGraphEngine>>;

// ── Tuning constants ──────────────────────────────────────────────────────────

/// Default precision applied to a market price observed in a `MarketUpdate`.
///
/// 0.7 expresses moderate trust: market price contributes 70 % of the
/// fusion log-odds, leaving 30 % to the model prior and other evidence.
const DEFAULT_MARKET_PRECISION: f64 = 0.7;

/// Strength applied to [`GraphPropagation`](EvidenceSource::GraphPropagation)
/// evidence arriving directly from `GraphUpdate.node_updates`.
const GRAPH_EVIDENCE_STRENGTH: f64 = 0.5;

/// Strength applied to secondary downstream graph influence (edges from graph
/// engine not directly in `node_updates`).
const GRAPH_DOWNSTREAM_STRENGTH: f64 = 0.25;

/// Strength applied to [`NewsSentiment`](EvidenceSource::NewsSentiment)
/// evidence from `SentimentUpdate` events.
const SENTIMENT_EVIDENCE_STRENGTH: f64 = 0.4;

// ── Internal helper ───────────────────────────────────────────────────────────

/// Build a [`PosteriorUpdate`] payload from a snapshot of a [`BeliefState`].
///
/// Must be called while holding a write lock (so `posterior_prob` is fresh).
fn build_posterior_update(belief: &BeliefState) -> PosteriorUpdate {
    let reference  = belief.market_prob.unwrap_or(belief.prior_prob);
    let n          = belief.update_history.len();
    // Evidence-density confidence: how much history the model has accumulated.
    // sqrt(n / MAX_HISTORY) grows from 0 → 1 as evidence accumulates.
    let confidence = ((n as f64) / (MAX_HISTORY as f64)).sqrt().clamp(0.0, 1.0);
    PosteriorUpdate {
        market_id:      belief.market_id.clone(),
        prior_prob:     belief.prior_prob,
        posterior_prob: belief.posterior_prob,
        market_prob:    belief.market_prob,
        deviation:      belief.posterior_prob - reference,
        confidence,
        timestamp:      Utc::now(),
    }
}

// ── BayesianModel ─────────────────────────────────────────────────────────────

/// Async wrapper that connects a [`BayesianEngine`] to the system Event Bus.
///
/// # Event subscriptions
///
/// | Subscribes to          | Action                                              |
/// |------------------------|-----------------------------------------------------|
/// | `Event::Market(…)`    | Seeds/updates prior; fuses market price             |
/// | `Event::Graph(…)`     | Ingests graph-propagated implied-prob changes       |
/// | `Event::Sentiment(…)` | Ingests news sentiment as log-odds evidence         |
///
/// # Events published
///
/// | Event                  | When                                                |
/// |------------------------|-----------------------------------------------------|
/// | `Event::Posterior(…)` | After each affected market's posterior is computed  |
///
/// # Graph integration (optional)
///
/// When constructed via [`with_graph`](Self::with_graph), the `GraphUpdate`
/// handler additionally reads edge weights from the graph engine to apply
/// downstream influence to markets adjacent to those listed in `node_updates`.
pub struct BayesianModel {
    engine: SharedBayesianEngine,
    /// Optional graph engine reference for edge-weight-guided influence.
    graph: Option<SharedGraphEngine>,
    bus: EventBus,
}

impl BayesianModel {
    /// Create a standalone model backed by an empty [`BayesianEngine`].
    ///
    /// Beliefs are seeded dynamically as `MarketUpdate` events arrive.
    pub fn new(bus: EventBus) -> Self {
        Self {
            engine: Arc::new(RwLock::new(BayesianEngine::new())),
            graph: None,
            bus,
        }
    }

    /// Create a model that also reads from a shared [`MarketGraphEngine`] to
    /// propagate upstream belief changes to downstream neighbors.
    pub fn with_graph(graph: SharedGraphEngine, bus: EventBus) -> Self {
        Self {
            engine: Arc::new(RwLock::new(BayesianEngine::new())),
            graph: Some(graph),
            bus,
        }
    }

    /// Create a wrapper around an already-constructed shared engine
    /// (e.g. one pre-seeded via [`BayesianEngine::from_graph`]).
    pub fn with_engine(engine: SharedBayesianEngine, bus: EventBus) -> Self {
        Self { engine, graph: None, bus }
    }

    /// Clone the inner engine handle for external read/write access from
    /// other tasks (e.g. arb_agent reading posteriors without the event bus).
    pub fn engine_handle(&self) -> SharedBayesianEngine {
        Arc::clone(&self.engine)
    }

    // ── Event handlers ────────────────────────────────────────────────────────

    /// `Event::Market` handler — seeds or updates the prior and fuses the
    /// new market price, then publishes a `PosteriorUpdate`.
    async fn on_market_update(&self, update: &common::MarketUpdate) {
        counter!("bayesian_engine_market_updates_total").increment(1);
        let market_id   = &update.market.id;
        let market_prob = update.market.probability;

        let payload: Option<PosteriorUpdate> = {
            let mut eng = self.engine.write().await;
            eng.ensure_belief(market_id, market_prob);

            match eng.fuse_market_price(market_id, market_prob, DEFAULT_MARKET_PRECISION) {
                Ok(()) => {
                    let _ = eng.compute_posterior(market_id);
                    eng.get_belief(market_id).map(build_posterior_update)
                }
                Err(e) => {
                    warn!("bayesian_engine: fuse_market_price failed for {market_id}: {e}");
                    None
                }
            }
        };

        if let Some(p) = payload {
            debug!(
                market_id,
                posterior = p.posterior_prob,
                deviation = p.deviation,
                "bayesian_engine: MarketUpdate → PosteriorUpdate"
            );
            if let Err(e) = self.bus.publish(Event::Posterior(p)) {
                warn!("bayesian_engine: failed to publish PosteriorUpdate: {e}");
            } else {
                counter!("bayesian_engine_posterior_updates_emitted_total").increment(1);
            }
        }
    }

    /// `Event::Graph` handler — ingests implied-probability deltas from the
    /// propagation result as graph-influence evidence.
    ///
    /// Two-phase design to avoid holding multiple locks across await points:
    /// 1. **Read phase**: collect downstream edge data from the graph engine
    ///    (requires read locks on both `engine` and `graph`).
    /// 2. **Write phase**: apply all evidence and collect payloads under a
    ///    single engine write lock.
    async fn on_graph_update(&self, gu: &common::GraphUpdate) {
        counter!("bayesian_engine_graph_updates_total").increment(1);
        // ── Phase 1: collect downstream edge influence ─────────────────────────
        let downstream: Vec<(String, f64)> = if let Some(graph_arc) = &self.graph {
            let eng   = self.engine.read().await;
            let graph = graph_arc.read().await;
            let mut result = Vec::new();

            for nu in &gu.node_updates {
                let delta = nu.new_implied_prob - nu.old_implied_prob;
                if delta.abs() < 1e-6 { continue; }

                let upstream_conf = eng
                    .get_belief(&nu.market_id)
                    .map(|b| b.market_confidence)
                    .unwrap_or(DEFAULT_MARKET_PRECISION);

                if let Ok(dependents) = graph.get_dependent_nodes(&nu.market_id) {
                    for (downstream_id, eff_weight) in dependents {
                        let influence = eff_weight * delta * upstream_conf;
                        if influence.abs() < 1e-6 { continue; }
                        let ref_p    = nu.old_implied_prob.clamp(0.01, 0.99);
                        let lo_delta = delta_prob_to_log_odds(influence, ref_p);
                        result.push((downstream_id, lo_delta));
                    }
                }
            }
            result
        } else {
            Vec::new()
        };
        // Both read locks are dropped here.

        // ── Phase 2: apply evidence under write lock ───────────────────────────
        let mut payloads: Vec<PosteriorUpdate> = Vec::new();
        {
            let mut eng = self.engine.write().await;

            // Primary updates: every node whose implied_prob changed.
            for nu in &gu.node_updates {
                let delta = nu.new_implied_prob - nu.old_implied_prob;
                if delta.abs() < 1e-6 { continue; }

                // Seed the belief if this is the first time we've seen this market
                // (it may not have had a MarketUpdate yet).
                eng.ensure_belief(&nu.market_id, nu.new_implied_prob);

                let ref_p    = nu.old_implied_prob.clamp(0.01, 0.99);
                let lo_delta = delta_prob_to_log_odds(delta, ref_p);

                if let Err(e) = eng.ingest_evidence(
                    &nu.market_id,
                    lo_delta,
                    EvidenceSource::GraphPropagation,
                    GRAPH_EVIDENCE_STRENGTH,
                ) {
                    warn!("bayesian_engine: graph ingest_evidence failed for {}: {e}", nu.market_id);
                    continue;
                }

                let _ = eng.compute_posterior(&nu.market_id);
                if let Some(b) = eng.get_belief(&nu.market_id) {
                    payloads.push(build_posterior_update(b));
                }
            }

            // Secondary updates: downstream neighbors reached via graph edges.
            for (market_id, lo_delta) in &downstream {
                // Only update markets already tracked — we don't seed from edges.
                if eng.get_belief(market_id).is_none() { continue; }

                if let Err(e) = eng.ingest_evidence(
                    market_id,
                    *lo_delta,
                    EvidenceSource::GraphPropagation,
                    GRAPH_DOWNSTREAM_STRENGTH,
                ) {
                    warn!("bayesian_engine: downstream ingest_evidence failed for {market_id}: {e}");
                    continue;
                }

                let _ = eng.compute_posterior(market_id);
                if !payloads.iter().any(|p| p.market_id == *market_id) {
                    if let Some(b) = eng.get_belief(market_id) {
                        payloads.push(build_posterior_update(b));
                    }
                }
            }
        } // write lock released

        // ── Phase 3: publish outside the lock ─────────────────────────────────
        for p in payloads {
            trace!(
                market_id = %p.market_id,
                posterior = p.posterior_prob,
                deviation = p.deviation,
                "bayesian_engine: GraphUpdate → PosteriorUpdate"
            );
            if let Err(e) = self.bus.publish(Event::Posterior(p)) {
                warn!("bayesian_engine: failed to publish PosteriorUpdate: {e}");
            } else {
                counter!("bayesian_engine_posterior_updates_emitted_total").increment(1);
            }
        }
    }

    /// `Event::Sentiment` handler — converts `sentiment_score ∈ [−1, 1]` to
    /// a log-odds contribution and ingests it for every related market.
    async fn on_sentiment_update(&self, sentiment: &common::SentimentUpdate) {
        counter!("bayesian_engine_sentiment_updates_total").increment(1);
        // sentiment_score is used directly as log-odds delta:
        //   +1.0 → 1 log-odd of positive evidence (~2.72× likelihood ratio)
        //   −1.0 → 1 log-odd of negative evidence
        let lo_delta = sentiment.sentiment_score;
        let mut payloads: Vec<PosteriorUpdate> = Vec::new();

        {
            let mut eng = self.engine.write().await;
            for market_id in &sentiment.related_market_ids {
                // News can update tracked markets but cannot seed new ones.
                if eng.get_belief(market_id).is_none() {
                    trace!(
                        market_id,
                        "bayesian_engine: SentimentUpdate for untracked market — skipping"
                    );
                    continue;
                }

                if let Err(e) = eng.ingest_evidence(
                    market_id,
                    lo_delta,
                    EvidenceSource::NewsSentiment,
                    SENTIMENT_EVIDENCE_STRENGTH,
                ) {
                    warn!("bayesian_engine: sentiment ingest_evidence failed for {market_id}: {e}");
                    continue;
                }

                let _ = eng.compute_posterior(market_id);
                if let Some(b) = eng.get_belief(market_id) {
                    payloads.push(build_posterior_update(b));
                }
            }
        }

        for p in payloads {
            debug!(
                market_id = %p.market_id,
                posterior = p.posterior_prob,
                sentiment_score = sentiment.sentiment_score,
                "bayesian_engine: SentimentUpdate → PosteriorUpdate"
            );
            if let Err(e) = self.bus.publish(Event::Posterior(p)) {
                warn!("bayesian_engine: failed to publish PosteriorUpdate: {e}");
            } else {
                counter!("bayesian_engine_posterior_updates_emitted_total").increment(1);
            }
        }
    }

    // ── Main event loop ───────────────────────────────────────────────────────

    /// Main event loop.
    ///
    /// Subscribes to `Market`, `Graph`, and `Sentiment` events on the bus and
    /// dispatches each to the appropriate handler.  Runs until `cancel` is
    /// triggered or the bus closes.
    pub async fn run(self, cancel: CancellationToken) {
        let mut rx: broadcast::Receiver<Arc<Event>> = self.bus.subscribe();
        info!("bayesian_engine: started — listening for Market / Graph / Sentiment events");

        loop {
            let event = tokio::select! {
                _ = cancel.cancelled() => {
                    info!("bayesian_engine: shutdown requested, exiting");
                    break;
                }
                result = rx.recv() => result,
            };

            match event {
                Ok(ev) => match ev.as_ref() {
                    Event::Market(update)      => self.on_market_update(update).await,
                    Event::Graph(gu)           => self.on_graph_update(gu).await,
                    Event::Sentiment(sentiment) => self.on_sentiment_update(sentiment).await,
                    _                          => {}
                },

                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(
                        "bayesian_engine: lagged by {n} events — \
                         consider increasing bus capacity or reducing tick rate"
                    );
                }

                Err(broadcast::error::RecvError::Closed) => {
                    info!("bayesian_engine: event bus closed, shutting down");
                    break;
                }
            }
        }
    }
}
