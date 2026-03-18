// crates/world_model/src/engine.rs
//
// World Model Engine — maintains a coherent global probabilistic world view.
//
// ## Pipeline position
//
// ```text
// BayesianEngine  ──→  Event::Posterior
//                           └─→ WorldModelEngine   ← this module
// MarketGraph     ──→  Event::Graph
//                           └─→ WorldModelEngine
// MarketScanner   ──→  Event::Market
//                           └─→ WorldModelEngine
//
// WorldModelEngine ──→  Event::WorldProbability  (to strategy agents / risk)
//                  ──→  Event::WorldSignal        (actionable edge signals)
//                  ──→  Event::Inconsistency      (constraint violations)
// ```
//
// ## Event handling
//
// | Event            | Action                                                  |
// |------------------|---------------------------------------------------------|
// | `Posterior`      | Update market variable, run inference, check constraints|
// | `Graph`          | Sync implied probs, run inference for source market     |
// | `Market`         | Seed/update market price, check constraints             |
//
// ## Thread safety
//
// `run()` consumes `self` and drives a single async task.  The shared state
// is `Arc<RwLock<>>` and locks are **never held across `.await` points**.
// All mutations plus snapshot building happen inside one write-lock scope;
// publishing happens after the lock is released.

use std::sync::Arc;

use chrono::Utc;
use common::{
    Event, EventBus, GraphUpdate, InconsistencyDetected, MarketUpdate, PosteriorUpdate,
    WorldProbabilityUpdate, WorldSignal,
};
use metrics::counter;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    config::WorldModelConfig,
    constraints::check_constraints,
    inference::propagate_update,
    types::{ConstraintViolation, MarketVariable, WorldInferenceResult, WorldState},
};

// ---------------------------------------------------------------------------
// Shared state type alias
// ---------------------------------------------------------------------------

pub type SharedWorldState = Arc<tokio::sync::RwLock<WorldState>>;

pub fn new_shared_state() -> SharedWorldState {
    Arc::new(tokio::sync::RwLock::new(WorldState::default()))
}

// ---------------------------------------------------------------------------
// Internal types built inside the write-lock and published after
// ---------------------------------------------------------------------------

/// A pending `WorldSignal` to be published after the lock is released.
/// Using a named struct avoids an untyped 5-tuple that could be mis-ordered.
struct PendingSignal {
    market_id: String,
    world_prob: f64,
    market_prob: f64,
    confidence: f64,
    edge: f64,
}

struct InferenceOutput {
    /// Payload for `Event::WorldProbability`.
    source_market_id: String,
    updated_probabilities: std::collections::HashMap<String, f64>,
    propagation_count: usize,
    /// Signals ready to emit.
    signals: Vec<PendingSignal>,
    /// Violations ready to emit.
    violations: Vec<ConstraintViolation>,
}

// ---------------------------------------------------------------------------
// WorldModelEngine
// ---------------------------------------------------------------------------

pub struct WorldModelEngine {
    pub config: WorldModelConfig,
    /// Shared mutable state — `pub` so tests can pre-seed or inspect.
    pub state: SharedWorldState,
    bus: EventBus,
    rx: Option<broadcast::Receiver<Arc<Event>>>,
}

impl WorldModelEngine {
    pub fn new(config: WorldModelConfig, bus: EventBus) -> Self {
        if let Err(e) = config.validate() {
            panic!("invalid WorldModelConfig: {e}");
        }
        let rx = bus.subscribe();
        Self {
            config,
            state: new_shared_state(),
            bus,
            rx: Some(rx),
        }
    }

    /// Clone the shared state handle for external inspection (e.g. health checks).
    pub fn state(&self) -> SharedWorldState {
        self.state.clone()
    }

    // -----------------------------------------------------------------------
    // Async event loop
    // -----------------------------------------------------------------------

    /// Run the engine until `cancel` fires or the bus closes.
    ///
    /// Consumes `self`; spawn as a Tokio task:
    /// ```ignore
    /// tokio::spawn(world_model.run(cancel.child_token()));
    /// ```
    pub async fn run(mut self, cancel: CancellationToken) {
        let mut rx = self.rx.take().expect("rx already consumed");
        info!("world_model: started");

        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    info!("world_model: cancelled, shutting down");
                    break;
                }
                result = rx.recv() => {
                    match result {
                        Ok(ev) => self.handle_event(ev.as_ref()).await,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("world_model: lagged by {n} events — consider increasing bus capacity");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("world_model: bus closed, shutting down");
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
        match event {
            Event::Posterior(update) => self.on_posterior(update).await,
            Event::Graph(update) => self.on_graph_update(update).await,
            Event::Market(update) => self.on_market_update(update).await,
            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // Posterior update handler
    // -----------------------------------------------------------------------

    async fn on_posterior(&self, update: &PosteriorUpdate) {
        counter!("world_model_posterior_updates_total").increment(1);

        // Validate incoming probability to guard against upstream bugs.
        if !(0.0..=1.0).contains(&update.posterior_prob) {
            warn!(
                market_id = %update.market_id,
                prob = update.posterior_prob,
                "world_model: ignoring out-of-range posterior probability"
            );
            return;
        }
        let confidence = update.confidence.clamp(0.0, 1.0);

        let output = {
            let mut state = self.state.write().await;

            // Capture old probability BEFORE modifying the entry.
            // This is required so that propagate_update receives a meaningful
            // delta (new − old).  Writing new_prob first then reading it back
            // would give delta_lo = 0 and suppress all propagation.
            let old_prob = state
                .variables
                .get(&update.market_id)
                .map(|v| v.current_probability)
                .unwrap_or(0.5); // neutral prior for markets the engine hasn't seen

            // Upsert market variable.
            let entry = state
                .variables
                .entry(update.market_id.clone())
                .or_insert_with(|| MarketVariable::new(&update.market_id, 0.5));

            entry.current_probability = update.posterior_prob;
            // Only overwrite cached market price when the upstream provides one;
            // if market_prob is None we keep whichever price we observed earlier.
            if let Some(mp) = update.market_prob {
                entry.market_probability = Some(mp);
            }
            entry.confidence = confidence;
            entry.last_update_timestamp = Utc::now();

            // Run world-model inference using the delta from old → new.
            let mut result = WorldInferenceResult::default();
            propagate_update(
                &mut state,
                &update.market_id,
                old_prob,
                update.posterior_prob,
                &mut result,
                self.config.max_propagation_hops,
                self.config.propagation_damping,
                self.config.propagation_threshold,
            );
            state.last_inference_time = Some(Utc::now());

            // Check constraints.
            let violations = check_constraints(&state);

            // Collect signals for updated markets that have a known market price.
            let signals = Self::collect_signals(&state, &result, self.config.min_signal_edge);

            let propagation_count = result.influence_propagations.len();
            InferenceOutput {
                source_market_id: update.market_id.clone(),
                updated_probabilities: result.updated_probabilities,
                propagation_count,
                signals,
                violations,
            }
        };
        // Lock released — no .await held a lock.

        self.publish_output(output).await;
    }

    // -----------------------------------------------------------------------
    // Graph update handler
    // -----------------------------------------------------------------------

    async fn on_graph_update(&self, update: &GraphUpdate) {
        counter!("world_model_graph_updates_total").increment(1);

        if update.node_updates.is_empty() {
            return;
        }

        let output = {
            let mut state = self.state.write().await;

            // Capture the source market's old probability before any writes.
            let old_source_prob = state
                .variables
                .get(&update.source_market_id)
                .map(|v| v.current_probability);

            // Sync implied probabilities from the market-graph propagation pass
            // into the world state, updating BOTH current_probability (so the
            // world model propagates a meaningful delta) and market_probability
            // (the reference price for edge signals).
            for node_update in &update.node_updates {
                let entry = state
                    .variables
                    .entry(node_update.market_id.clone())
                    .or_insert_with(|| {
                        MarketVariable::new(&node_update.market_id, node_update.new_implied_prob)
                    });
                entry.current_probability = node_update.new_implied_prob;
                entry.market_probability = Some(node_update.new_implied_prob);
                entry.last_update_timestamp = Utc::now();
            }

            // Run world-model inference from the source market.
            // We need old_prob (before this graph update) and new_prob (after).
            let Some(old_prob) = old_source_prob else {
                // Source market not in world state yet — nothing to propagate.
                return;
            };

            let new_prob = state
                .variables
                .get(&update.source_market_id)
                .map(|v| v.current_probability)
                .unwrap_or(old_prob);

            let mut result = WorldInferenceResult::default();
            propagate_update(
                &mut state,
                &update.source_market_id,
                old_prob,
                new_prob,
                &mut result,
                self.config.max_propagation_hops,
                self.config.propagation_damping,
                self.config.propagation_threshold,
            );
            state.last_inference_time = Some(Utc::now());

            let violations = check_constraints(&state);
            let signals = Self::collect_signals(&state, &result, self.config.min_signal_edge);
            let propagation_count = result.influence_propagations.len();

            InferenceOutput {
                source_market_id: update.source_market_id.clone(),
                updated_probabilities: result.updated_probabilities,
                propagation_count,
                signals,
                violations,
            }
        };

        self.publish_output(output).await;
    }

    // -----------------------------------------------------------------------
    // Market update handler — seed market price and check constraints
    // -----------------------------------------------------------------------

    async fn on_market_update(&self, update: &MarketUpdate) {
        let violations = {
            let mut state = self.state.write().await;
            let entry = state
                .variables
                .entry(update.market.id.clone())
                .or_insert_with(|| {
                    MarketVariable::new(&update.market.id, update.market.probability)
                });

            // Track the raw market price for edge computation.
            entry.market_probability = Some(update.market.probability);
            entry.last_update_timestamp = Utc::now();

            // Check constraints — a raw market price spike can cause a violation
            // even without a Posterior update (e.g. two candidates both high).
            check_constraints(&state)
        };
        // Lock released.

        self.publish_violations(violations).await;
    }

    // -----------------------------------------------------------------------
    // Signal collection (pure, called inside write-lock)
    // -----------------------------------------------------------------------

    /// For each market in `result.updated_probabilities` that has a known
    /// raw market price, compute the world edge and add a signal entry if the
    /// edge exceeds `min_edge`.
    fn collect_signals(
        state: &WorldState,
        result: &WorldInferenceResult,
        min_edge: f64,
    ) -> Vec<PendingSignal> {
        result
            .updated_probabilities
            .iter()
            .filter_map(|(market_id, &world_prob)| {
                let var = state.variables.get(market_id.as_str())?;
                let market_prob = var.market_probability?;
                let edge = world_prob - market_prob;
                if edge.abs() < min_edge {
                    return None;
                }
                Some(PendingSignal {
                    market_id: market_id.clone(),
                    world_prob,
                    market_prob,
                    confidence: var.confidence,
                    edge,
                })
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // Publishing (called after lock is released)
    // -----------------------------------------------------------------------

    async fn publish_output(&self, output: InferenceOutput) {
        // ── WorldProbabilityUpdate ─────────────────────────────────────────
        if !output.updated_probabilities.is_empty() {
            counter!("world_model_probability_updates_emitted_total").increment(1);
            debug!(
                source   = %output.source_market_id,
                updated  = output.updated_probabilities.len(),
                props    = output.propagation_count,
                "world_model: inference complete"
            );
            let ev = WorldProbabilityUpdate {
                source_market_id: output.source_market_id.clone(),
                updated_probabilities: output.updated_probabilities,
                propagation_count: output.propagation_count,
                timestamp: Utc::now(),
            };
            if let Err(e) = self.bus.publish(Event::WorldProbability(ev)) {
                warn!("world_model: failed to publish WorldProbabilityUpdate: {e}");
            }
        }

        // ── WorldSignal per market with meaningful edge ────────────────────
        for sig in output.signals {
            counter!("world_model_signals_emitted_total").increment(1);
            debug!(
                market_id  = %sig.market_id,
                world_prob = sig.world_prob,
                market_prob = sig.market_prob,
                edge       = sig.edge,
                "world_model: emitting WorldSignal"
            );
            let ev = WorldSignal {
                market_id: sig.market_id,
                world_probability: sig.world_prob,
                market_probability: sig.market_prob,
                world_edge: sig.edge,
                confidence: sig.confidence,
                timestamp: Utc::now(),
            };
            if let Err(e) = self.bus.publish(Event::WorldSignal(ev)) {
                warn!("world_model: failed to publish WorldSignal: {e}");
            }
        }

        self.publish_violations(output.violations).await;
    }

    async fn publish_violations(&self, violations: Vec<ConstraintViolation>) {
        for violation in violations {
            counter!("world_model_inconsistencies_detected_total").increment(1);
            warn!(
                constraint_id = %violation.constraint_id,
                magnitude     = violation.violation_magnitude,
                "world_model: constraint violation detected"
            );
            let ev = InconsistencyDetected {
                constraint_id: violation.constraint_id,
                constraint_type: format!("{:?}", violation.constraint_type),
                markets_involved: violation.markets_involved,
                current_probabilities: violation.current_probabilities,
                violation_magnitude: violation.violation_magnitude,
                timestamp: Utc::now(),
            };
            if let Err(e) = self.bus.publish(Event::Inconsistency(ev)) {
                warn!("world_model: failed to publish InconsistencyDetected: {e}");
            }
        }
    }
}
