// crates/scenario_engine/src/engine.rs
//
// Scenario Engine — generates probabilistic world scenarios via Monte Carlo
// sampling and detects cross-market mispricing.
//
// ## Pipeline position
//
// ```text
// WorldModelEngine ──→ Event::WorldProbability
//                           └─→ ScenarioEngine    ← this module
// MarketGraph      ──→ Event::Graph
//                           └─→ ScenarioEngine  (dependency inference only)
// MarketScanner    ──→ Event::Market
//                           └─→ ScenarioEngine  (price seeding)
// WorldModelEngine ──→ Event::WorldSignal
//                           └─→ ScenarioEngine  (confidence values)
//
// ScenarioEngine ──→ Event::ScenarioBatch          (to strategy agents)
//                ──→ Event::ScenarioExpectations   (to strategy agents)
//                ──→ Event::ScenarioSignal         (to strategy agents)
// ```
//
// ## Lock discipline
//
// All state mutations and snapshot building happen inside one write-lock scope.
// The CPU-intensive work (Monte Carlo + analysis) is dispatched to the thread
// pool via `tokio::task::spawn_blocking` so it never blocks the async runtime.
// Publishing happens after `spawn_blocking` completes and the lock has been
// long released.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use common::{
    Event, EventBus, GraphUpdate, MarketUpdate, ScenarioBatchGenerated, ScenarioExpectations,
    ScenarioSignal, WorldProbabilityUpdate, WorldSignal,
};
use metrics::counter;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    analysis::{compute_expectations, detect_mispricing, top_joint_pairs},
    config::ScenarioEngineConfig,
    sampler::sample_scenarios,
    types::{DependencyEdge, MarketId, PendingSignal, ScenarioEngineState},
};

// ---------------------------------------------------------------------------
// Shared state type alias
// ---------------------------------------------------------------------------

pub type SharedScenarioState = Arc<tokio::sync::RwLock<ScenarioEngineState>>;

pub fn new_shared_state() -> SharedScenarioState {
    Arc::new(tokio::sync::RwLock::new(ScenarioEngineState::default()))
}

// ---------------------------------------------------------------------------
// Internal snapshot — moved into spawn_blocking closure for CPU-intensive work
// ---------------------------------------------------------------------------

/// Owned snapshot of all state needed for a sampling pass.
/// All fields must be `Send + 'static` for `spawn_blocking`.
struct StateSnapshot {
    beliefs: HashMap<MarketId, f64>,
    dependencies: Vec<DependencyEdge>,
    market_prices: HashMap<MarketId, f64>,
    confidence_map: HashMap<MarketId, f64>,
}

/// Result returned from `spawn_blocking` after sampling + analysis.
struct BatchResult {
    market_count: usize,
    sample_size: usize,
    generation_time_ms: u64,
    expected_probabilities: HashMap<String, f64>,
    joint_probabilities: HashMap<String, f64>,
    signals: Vec<PendingSignal>,
}

// ---------------------------------------------------------------------------
// ScenarioEngine
// ---------------------------------------------------------------------------

pub struct ScenarioEngine {
    pub config: ScenarioEngineConfig,
    /// Shared mutable state; `pub` so tests can pre-seed or inspect.
    pub state: SharedScenarioState,
    bus: EventBus,
    /// Subscription stored as `Option` so `run()` can `.take()` it without
    /// needing `&mut self`.  Subscribed eagerly in `new()` to eliminate
    /// subscribe-after-spawn race conditions.
    rx: Option<broadcast::Receiver<Arc<Event>>>,
}

impl ScenarioEngine {
    pub fn new(config: ScenarioEngineConfig, bus: EventBus) -> Self {
        if let Err(e) = config.validate() {
            panic!("invalid ScenarioEngineConfig: {e}");
        }
        let rx = bus.subscribe();
        Self {
            config,
            state: new_shared_state(),
            bus,
            rx: Some(rx),
        }
    }

    /// Clone the shared state handle for external inspection.
    pub fn state(&self) -> SharedScenarioState {
        self.state.clone()
    }

    // -----------------------------------------------------------------------
    // Async event loop
    // -----------------------------------------------------------------------

    pub async fn run(mut self, cancel: CancellationToken) {
        let mut rx = self.rx.take().expect("rx already consumed");
        info!("scenario_engine: started");

        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    info!("scenario_engine: cancelled, shutting down");
                    break;
                }
                result = rx.recv() => {
                    match result {
                        Ok(ev) => self.handle_event(ev.as_ref()).await,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(
                                "scenario_engine: lagged by {n} events \
                                 — consider increasing bus capacity"
                            );
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("scenario_engine: bus closed, shutting down");
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
            Event::WorldProbability(update) => self.on_world_probability(update).await,
            Event::Graph(update) => self.on_graph_update(update).await,
            Event::Market(update) => self.on_market_update(update).await,
            Event::WorldSignal(signal) => self.on_world_signal(signal).await,
            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // WorldProbabilityUpdate — primary trigger for scenario generation
    // -----------------------------------------------------------------------

    async fn on_world_probability(&self, update: &WorldProbabilityUpdate) {
        counter!("scenario_engine_world_probability_updates_total").increment(1);

        let snapshot = {
            let mut state = self.state.write().await;

            // Merge updated world-model probabilities, rejecting invalid values.
            for (market_id, &prob) in &update.updated_probabilities {
                if prob.is_finite() && (0.0..=1.0).contains(&prob) {
                    state.world_probabilities.insert(market_id.clone(), prob);
                } else {
                    warn!(
                        market_id = %market_id,
                        prob,
                        "scenario_engine: ignoring invalid world probability from WorldProbabilityUpdate"
                    );
                }
            }
            state.generation += 1;

            // Snapshot for sampling (happens outside the lock).
            StateSnapshot {
                beliefs: state.world_probabilities.clone(),
                dependencies: state.dependencies.clone(),
                market_prices: state.market_prices.clone(),
                confidence_map: state.confidence_map.clone(),
            }
        };
        // Lock released — safe to do CPU-intensive work now.

        if snapshot.beliefs.is_empty() {
            return;
        }

        self.generate_and_publish(snapshot).await;
    }

    // -----------------------------------------------------------------------
    // GraphUpdate — infer dependency edges only; do NOT overwrite world probs.
    //
    // `world_probabilities` is populated from `WorldProbabilityUpdate` (the
    // world model's refined belief after global propagation).  `GraphUpdate`
    // carries raw BFS-propagation outputs — inputs to the world model, not
    // its final beliefs.  Writing those raw values here would race with the
    // world model's refined update arriving moments later, potentially
    // replacing a refined posterior with a stale implied probability.
    // -----------------------------------------------------------------------

    async fn on_graph_update(&self, update: &GraphUpdate) {
        counter!("scenario_engine_graph_updates_total").increment(1);

        if update.node_updates.is_empty() {
            return;
        }

        let max_deps = self.config.max_dependencies;
        let mut state = self.state.write().await;

        for node_update in &update.node_updates {
            // Infer a signed dependency edge from source → updated node.
            // Weight is estimated from the direction of the implied-prob change;
            // magnitude is capped at 0.5 to keep adjustments conservative.
            let raw_delta = node_update.new_implied_prob - node_update.old_implied_prob;
            if raw_delta.abs() > 1e-6 {
                let weight = (raw_delta * 2.0).clamp(-0.5, 0.5);
                upsert_dependency(
                    &mut state.dependencies,
                    update.source_market_id.clone(),
                    node_update.market_id.clone(),
                    weight,
                    max_deps,
                );
            }
        }
        state.generation += 1;
    }

    // -----------------------------------------------------------------------
    // MarketUpdate — track raw market prices for mispricing detection
    // -----------------------------------------------------------------------

    async fn on_market_update(&self, update: &MarketUpdate) {
        counter!("scenario_engine_market_updates_total").increment(1);

        let prob = update.market.probability;
        if !prob.is_finite() || !(0.0..=1.0).contains(&prob) {
            warn!(
                market_id = %update.market.id,
                prob,
                "scenario_engine: ignoring invalid market probability from MarketUpdate"
            );
            return;
        }

        let mut state = self.state.write().await;
        state.market_prices.insert(update.market.id.clone(), prob);
        // Seed the belief map so scenarios include this market even before the
        // world model emits a WorldProbabilityUpdate for it.
        state
            .world_probabilities
            .entry(update.market.id.clone())
            .or_insert(prob);
    }

    // -----------------------------------------------------------------------
    // WorldSignal — extract per-market confidence values
    // -----------------------------------------------------------------------

    async fn on_world_signal(&self, signal: &WorldSignal) {
        let confidence = signal.confidence.clamp(0.0, 1.0);
        let mut state = self.state.write().await;
        state
            .confidence_map
            .insert(signal.market_id.clone(), confidence);
    }

    // -----------------------------------------------------------------------
    // Core: sample → analyse → publish (all outside the lock)
    // -----------------------------------------------------------------------

    async fn generate_and_publish(&self, snapshot: StateSnapshot) {
        let sample_size = self.config.sample_size;
        let max_joint = self.config.max_joint_market_pairs;
        let threshold = self.config.min_mispricing_threshold;

        // Offload Monte Carlo + analysis to the blocking thread pool so the
        // async runtime is not starved during CPU-intensive computation.
        let result = tokio::task::spawn_blocking(move || {
            let market_count = snapshot.beliefs.len();
            let batch = sample_scenarios(&snapshot.beliefs, &snapshot.dependencies, sample_size);
            let gen_ms = batch.generation_time_ms;
            let actual_sample_size = batch.sample_size;

            let expectation = compute_expectations(&batch);
            let joint_map = top_joint_pairs(&expectation, max_joint);
            let signals = detect_mispricing(
                &expectation,
                &snapshot.market_prices,
                &snapshot.confidence_map,
                threshold,
            );

            BatchResult {
                market_count,
                sample_size: actual_sample_size,
                generation_time_ms: gen_ms,
                expected_probabilities: expectation.expected_probabilities,
                joint_probabilities: joint_map,
                signals,
            }
        })
        .await;

        let result = match result {
            Ok(r) => r,
            Err(e) => {
                warn!("scenario_engine: batch generation panicked: {e}");
                return;
            }
        };

        let batch_id = Utc::now().timestamp_micros().to_string();
        debug!(
            markets  = result.market_count,
            samples  = result.sample_size,
            gen_ms   = result.generation_time_ms,
            signals  = result.signals.len(),
            "scenario_engine: batch complete"
        );

        self.publish_batch_generated(
            &batch_id,
            result.sample_size,
            result.market_count,
            result.generation_time_ms,
        );
        self.publish_expectations(
            &batch_id,
            result.expected_probabilities,
            result.joint_probabilities,
        );
        self.publish_signals(result.signals);
    }

    // -----------------------------------------------------------------------
    // Publishing helpers
    // -----------------------------------------------------------------------

    fn publish_batch_generated(
        &self,
        batch_id: &str,
        sample_size: usize,
        market_count: usize,
        generation_time_ms: u64,
    ) {
        counter!("scenario_engine_batches_generated_total").increment(1);
        let ev = ScenarioBatchGenerated {
            batch_id: batch_id.to_string(),
            sample_size,
            market_count,
            generation_time_ms,
            timestamp: Utc::now(),
        };
        if let Err(e) = self.bus.publish(Event::ScenarioBatch(ev)) {
            warn!("scenario_engine: failed to publish ScenarioBatchGenerated: {e}");
        }
    }

    fn publish_expectations(
        &self,
        batch_id: &str,
        expected_probabilities: HashMap<String, f64>,
        joint_probabilities: HashMap<String, f64>,
    ) {
        counter!("scenario_engine_expectations_published_total").increment(1);
        let ev = ScenarioExpectations {
            batch_id: batch_id.to_string(),
            expected_probabilities,
            joint_probabilities,
            timestamp: Utc::now(),
        };
        if let Err(e) = self.bus.publish(Event::ScenarioExpectations(ev)) {
            warn!("scenario_engine: failed to publish ScenarioExpectations: {e}");
        }
    }

    fn publish_signals(&self, signals: Vec<PendingSignal>) {
        for sig in signals {
            counter!("scenario_engine_signals_emitted_total").increment(1);
            debug!(
                market_id   = %sig.market_id,
                expected    = sig.expected_probability,
                market      = sig.market_probability,
                mispricing  = sig.mispricing,
                "scenario_engine: emitting ScenarioSignal"
            );
            let ev = ScenarioSignal {
                market_id: sig.market_id,
                expected_probability: sig.expected_probability,
                market_probability: sig.market_probability,
                mispricing: sig.mispricing,
                confidence: sig.confidence,
                timestamp: Utc::now(),
            };
            if let Err(e) = self.bus.publish(Event::ScenarioSignal(ev)) {
                warn!("scenario_engine: failed to publish ScenarioSignal: {e}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Insert or update the dependency edge for `(parent, child)`.
///
/// Duplicate `(parent, child)` pairs are updated in-place via exponential
/// smoothing (70% old, 30% new) to prevent noisy estimates from dominating.
///
/// When `max_deps > 0` and the Vec is at capacity, the edge with the smallest
/// absolute weight (least informative) is evicted before inserting the new one.
///
/// Self-loops (`parent == child`) are silently ignored.
fn upsert_dependency(
    deps: &mut Vec<DependencyEdge>,
    parent: MarketId,
    child: MarketId,
    weight: f64,
    max_deps: usize,
) {
    if parent == child {
        return; // no self-loops
    }

    if let Some(existing) = deps
        .iter_mut()
        .find(|d| d.parent == parent && d.child == child)
    {
        // Blend: preserves trend while damping noise.
        existing.weight = (existing.weight * 0.7 + weight * 0.3).clamp(-1.0, 1.0);
        return;
    }

    // Evict weakest edge if at capacity.
    if max_deps > 0 && deps.len() >= max_deps {
        if let Some(min_pos) = deps
            .iter()
            .enumerate()
            .min_by(|a, b| {
                a.1.weight
                    .abs()
                    .partial_cmp(&b.1.weight.abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
        {
            deps.swap_remove(min_pos);
        }
    }

    deps.push(DependencyEdge {
        parent,
        child,
        weight,
    });
}
