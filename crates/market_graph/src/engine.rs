// crates/market_graph/src/engine.rs
// MarketGraphEngine — synchronous core of the Market Graph module.
//
// Thread safety: callers should wrap in `Arc<RwLock<MarketGraphEngine>>`.

use std::collections::{HashMap, VecDeque};

use chrono::Utc;
use petgraph::stable_graph::{EdgeIndex, NodeIndex, StableDiGraph};
use petgraph::visit::{EdgeRef, IntoEdgeReferences};
use petgraph::Direction;
use tracing::{debug, info, warn};

use crate::{
    error::GraphError,
    types::{
        EdgeType, GraphSnapshot, MarketEdge, MarketNode, NodeChange, PropagationConfig,
        PropagationResult, UpdateSource,
    },
};

// ── Validation helpers ────────────────────────────────────────────────────────

#[inline]
fn validate_prob(p: f64) -> Result<(), GraphError> {
    if (0.0..=1.0).contains(&p) {
        Ok(())
    } else {
        Err(GraphError::InvalidProbability(p))
    }
}

#[inline]
fn validate_weight(w: f64) -> Result<(), GraphError> {
    if (-1.0..=1.0).contains(&w) {
        Ok(())
    } else {
        Err(GraphError::InvalidWeight(w))
    }
}

#[inline]
fn validate_confidence(c: f64) -> Result<(), GraphError> {
    if (0.0..=1.0).contains(&c) {
        Ok(())
    } else {
        Err(GraphError::InvalidConfidence(c))
    }
}

// ── MarketGraphEngine ─────────────────────────────────────────────────────────

/// The synchronous Market Graph Engine.
///
/// Internally backed by a `petgraph::StableDiGraph` so node indices remain
/// valid after removals. All public methods are `O(1)` or `O(E)` in the
/// number of edges and are safe to call from a tokio task (no blocking I/O).
///
/// # Propagation algorithm
///
/// See [`MarketGraphEngine::propagate`] for full documentation.
///
/// # Example
///
/// ```rust
/// use market_graph::{MarketGraphEngine, MarketNode, EdgeType, UpdateSource};
///
/// let mut engine = MarketGraphEngine::new();
/// engine.add_market(MarketNode::new("trump_win", "Trump wins 2028", 0.61, None, None));
/// engine.add_market(MarketNode::new("gop_senate", "Republicans control Senate", 0.55, None, None));
/// engine.add_dependency("trump_win", "gop_senate", 0.42, 0.85, EdgeType::Statistical).unwrap();
///
/// engine.update_and_propagate("trump_win", 0.71, UpdateSource::Api).unwrap();
///
/// let dev = engine.get_deviation("gop_senate").unwrap();
/// println!("Deviation on GOP Senate: {:.4}", dev);
/// ```
pub struct MarketGraphEngine {
    /// Directed graph: nodes are MarketNode, edges are MarketEdge.
    /// `StableDiGraph` keeps NodeIndex values stable after removals.
    graph: StableDiGraph<MarketNode, MarketEdge>,

    /// Fast lookup: market_id → NodeIndex
    index_map: HashMap<String, NodeIndex>,

    /// Propagation tuning parameters
    pub config: PropagationConfig,
}

impl Default for MarketGraphEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl MarketGraphEngine {
    /// Create an engine with default propagation configuration.
    pub fn new() -> Self {
        Self::with_config(PropagationConfig::default())
    }

    /// Create an engine with custom propagation configuration.
    pub fn with_config(config: PropagationConfig) -> Self {
        Self {
            graph: StableDiGraph::new(),
            index_map: HashMap::new(),
            config,
        }
    }

    // ── Node operations ───────────────────────────────────────────────────────

    /// Insert a new market or update an existing one in place.
    ///
    /// If the market already exists its `implied_prob` and `our_position` are
    /// preserved (they are engine-managed fields, not sourced from market data).
    pub fn add_market(&mut self, node: MarketNode) -> NodeIndex {
        if let Some(&existing_idx) = self.index_map.get(&node.market_id) {
            // Preserve engine-managed fields
            let saved_implied = self.graph[existing_idx].implied_prob;
            let saved_position = self.graph[existing_idx].our_position;
            self.graph[existing_idx] = node;
            self.graph[existing_idx].implied_prob = saved_implied;
            self.graph[existing_idx].our_position = saved_position;
            existing_idx
        } else {
            let id = node.market_id.clone();
            let idx = self.graph.add_node(node);
            self.index_map.insert(id, idx);
            idx
        }
    }

    /// Remove a market and all edges incident to it.
    ///
    /// Any downstream node's `implied_prob` will no longer receive updates
    /// from the removed node after this call.
    pub fn remove_market(&mut self, market_id: &str) -> Result<(), GraphError> {
        let idx = self.node_idx(market_id)?;
        self.graph.remove_node(idx);
        self.index_map.remove(market_id);
        Ok(())
    }

    /// Immutable reference to a market node.
    #[must_use = "returns the node without modifying the graph"]
    pub fn get_market(&self, market_id: &str) -> Result<&MarketNode, GraphError> {
        Ok(&self.graph[self.node_idx(market_id)?])
    }

    /// Mutable reference to a market node.
    pub fn get_market_mut(&mut self, market_id: &str) -> Result<&mut MarketNode, GraphError> {
        let idx = self.node_idx(market_id)?;
        Ok(&mut self.graph[idx])
    }

    /// All market IDs currently in the graph.
    #[must_use = "returns the IDs without modifying the graph"]
    pub fn market_ids(&self) -> Vec<&str> {
        self.index_map.keys().map(String::as_str).collect()
    }

    /// Number of markets (nodes).
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Number of dependency edges.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    // ── Edge operations ───────────────────────────────────────────────────────

    /// Add or update a directed dependency edge: `from_id → to_id`.
    ///
    /// If the edge already exists, its weight, confidence, and type are
    /// updated in place (preserving `created_at` for decay calculation).
    pub fn add_dependency(
        &mut self,
        from_id: &str,
        to_id: &str,
        weight: f64,
        confidence: f64,
        edge_type: EdgeType,
    ) -> Result<EdgeIndex, GraphError> {
        validate_weight(weight)?;
        validate_confidence(confidence)?;

        let from_idx = self.node_idx(from_id)?;
        let to_idx = self.node_idx(to_id)?;

        if let Some(edge_idx) = self.graph.find_edge(from_idx, to_idx) {
            // Update in place — preserve created_at for decay continuity
            let e = &mut self.graph[edge_idx];
            e.weight = weight;
            e.confidence = confidence;
            e.edge_type = edge_type;
            debug!(from_id, to_id, weight, confidence, "add_dependency: updated existing edge");
            return Ok(edge_idx);
        }

        let edge = MarketEdge::new(from_id, to_id, weight, confidence, edge_type);
        let idx = self.graph.add_edge(from_idx, to_idx, edge);
        debug!(from_id, to_id, weight, confidence, "add_dependency: new edge added");
        Ok(idx)
    }

    /// Remove a dependency edge.
    pub fn remove_dependency(&mut self, from_id: &str, to_id: &str) -> Result<(), GraphError> {
        let from_idx = self.node_idx(from_id)?;
        let to_idx = self.node_idx(to_id)?;
        match self.graph.find_edge(from_idx, to_idx) {
            Some(edge_idx) => {
                self.graph.remove_edge(edge_idx);
                Ok(())
            }
            None => Err(GraphError::EdgeNotFound(from_id.to_string(), to_id.to_string())),
        }
    }

    // ── Probability updates ───────────────────────────────────────────────────

    /// Update a market's `current_prob` without triggering propagation.
    ///
    /// Returns the signed delta (`new_prob − old_prob`).
    pub fn update_market_prob(
        &mut self,
        market_id: &str,
        new_prob: f64,
        source: UpdateSource,
    ) -> Result<f64, GraphError> {
        validate_prob(new_prob)?;
        let idx = self.node_idx(market_id)?;
        let old = self.graph[idx].current_prob;
        self.graph[idx].current_prob = new_prob;
        self.graph[idx].last_source = source;      // Copy — no .clone() needed
        self.graph[idx].last_update = Utc::now();
        let delta = new_prob - old;
        debug!(market_id, old_prob = old, new_prob, delta, source = ?source, "update_market_prob");
        Ok(delta)
    }

    /// Update a market's probability **and** immediately propagate.
    ///
    /// If the delta is smaller than `config.change_threshold` the propagation
    /// pass is skipped and a zero-cost result is returned.
    pub fn update_and_propagate(
        &mut self,
        market_id: &str,
        new_prob: f64,
        source: UpdateSource,
    ) -> Result<PropagationResult, GraphError> {
        let delta = self.update_market_prob(market_id, new_prob, source)?;
        if delta.abs() < self.config.change_threshold {
            return Ok(PropagationResult {
                nodes_updated: 0,
                iterations: 0,
                converged: true,
                updated_node_ids: vec![],
                node_changes: vec![],
            });
        }
        self.propagate(Some(market_id))
    }

    /// Apply multiple probability updates and run a single propagation pass.
    ///
    /// More efficient than calling `update_and_propagate` N times when many
    /// markets change simultaneously (e.g. after a batch API response).
    pub fn batch_update_and_propagate(
        &mut self,
        updates: &[(&str, f64, UpdateSource)],
    ) -> Result<PropagationResult, GraphError> {
        for &(id, prob, src) in updates {   // UpdateSource is Copy — no ref needed
            self.update_market_prob(id, prob, src)?;
        }
        self.propagate(None)
    }

    // ── Propagation ───────────────────────────────────────────────────────────

    /// Propagate probability changes through the dependency graph and update
    /// `implied_prob` on all reachable downstream nodes.
    ///
    /// ## Algorithm: Depth-Limited BFS Message Passing
    ///
    /// ### Step 1 — Seed the BFS queue
    ///
    /// * If `from_market_id` is `Some(id)`:
    ///   - `delta = id.current_prob − id.implied_prob`
    ///   - Skip entirely if `|delta| < change_threshold` (sparse propagation)
    ///   - Set `id.implied_prob = id.current_prob` (source is ground truth)
    ///   - Enqueue `(id, delta, depth=0)`
    ///
    /// * If `None` (full-graph pass):
    ///   - Collect every node where `|current_prob − implied_prob| >= change_threshold`
    ///   - Reset each source node's `implied_prob` to `current_prob`
    ///   - Enqueue all such `(node, delta, depth=0)` entries
    ///
    /// ### Step 2 — BFS with depth limit
    ///
    /// ```text
    /// while queue not empty:
    ///     (node, Δ, depth) = queue.pop_front()
    ///     if depth >= max_depth → skip           // depth limiter
    ///     for each outgoing edge  node → target  with effective_weight w:
    ///         implied_change = w × Δ × damping_factor
    ///         if |implied_change| < convergence_threshold → skip  // sparse
    ///         target.implied_prob = clamp(target.implied_prob + implied_change, 0, 1)
    ///         actual_change = new_implied − old_implied
    ///         if |actual_change| >= convergence_threshold:
    ///             queue.push_back((target, actual_change, depth + 1))
    /// ```
    ///
    /// ### Termination guarantees
    ///
    /// 1. **Depth limit** (`max_depth`, default 3): any task at depth ≥ `max_depth`
    ///    is dropped, bounding the maximum propagation radius to local neighbours.
    /// 2. **Convergence threshold**: the `damping_factor` (default 0.85) attenuates
    ///    each message per hop.  For a cycle of length k with `d = 0.85`, the signal
    ///    after k hops is `d^k`.  Combined with `convergence_threshold = 0.001`,
    ///    the queue empties naturally well within the depth bound.
    ///
    /// ### Effective weight
    ///
    /// `effective_weight = edge.weight × edge.confidence × time_decay`
    ///
    /// Negative weights produce negative implied changes (anti-correlation).
    ///
    /// ### Complexity per update call
    ///
    /// `O(local_edges × max_depth)` — touches only nodes reachable from the
    /// seed within the depth budget, not the entire graph.
    pub fn propagate(
        &mut self,
        from_market_id: Option<&str>,
    ) -> Result<PropagationResult, GraphError> {
        // ── Internal BFS task ─────────────────────────────────────────────────
        struct PropagationTask {
            node_idx: NodeIndex,
            /// Signed delta that arrived at this node.
            delta: f64,
            /// BFS depth (0 = seed node).
            depth: usize,
        }

        // ── Build initial queue ───────────────────────────────────────────────
        let mut queue: VecDeque<PropagationTask> = VecDeque::new();

        match from_market_id {
            Some(id) => {
                let idx = self.node_idx(id)?;
                let node = &self.graph[idx];
                let delta = node.current_prob - node.implied_prob;

                if delta.abs() < self.config.change_threshold {
                    return Ok(PropagationResult {
                        nodes_updated: 0,
                        iterations: 0,
                        converged: true,
                        updated_node_ids: vec![],
                        node_changes: vec![],
                    });
                }

                // Source is ground truth — align implied_prob before propagating.
                self.graph[idx].implied_prob = self.graph[idx].current_prob;
                queue.push_back(PropagationTask { node_idx: idx, delta, depth: 0 });
            }

            None => {
                // Seed queue with every node whose implied_prob has drifted.
                let dirty: Vec<(NodeIndex, f64)> = self
                    .index_map
                    .values()
                    .filter_map(|&idx| {
                        let n = &self.graph[idx];
                        let delta = n.current_prob - n.implied_prob;
                        (delta.abs() >= self.config.change_threshold).then_some((idx, delta))
                    })
                    .collect();

                for &(idx, _) in &dirty {
                    let cp = self.graph[idx].current_prob;
                    self.graph[idx].implied_prob = cp;
                }
                for (idx, delta) in dirty {
                    queue.push_back(PropagationTask { node_idx: idx, delta, depth: 0 });
                }
            }
        }

        // ── BFS with depth limit ──────────────────────────────────────────────
        //
        // For each updated node we record (original_implied, latest_implied).
        // `original_implied` is locked in on the first visit; `latest_implied`
        // is overwritten on each subsequent contribution (diamond patterns).
        let mut node_probs: HashMap<NodeIndex, (f64, f64)> = HashMap::new();
        let mut iterations = 0usize;
        let mut depth_limited = false;

        // Capture wall-clock time once — avoids a syscall per edge.
        let now = Utc::now();

        while let Some(task) = queue.pop_front() {
            if task.depth >= self.config.max_depth {
                depth_limited = true;
                continue;
            }

            iterations += 1;

            // Collect outgoing edges before mutating the graph (borrow-checker).
            let outgoing: Vec<(NodeIndex, f64)> = self
                .graph
                .edges(task.node_idx)
                .map(|e| (e.target(), e.weight().effective_weight_at(now)))
                .collect();

            for (target_idx, eff_weight) in outgoing {
                let implied_change = eff_weight * task.delta * self.config.damping_factor;

                if implied_change.abs() < self.config.convergence_threshold {
                    continue;
                }

                let old_implied = self.graph[target_idx].implied_prob;
                let new_implied = (old_implied + implied_change).clamp(0.0, 1.0);
                let actual_change = new_implied - old_implied;

                if actual_change.abs() < self.config.convergence_threshold {
                    continue;
                }

                // Preserve pre-propagation baseline on first touch; update
                // `latest` on every subsequent contribution.
                node_probs
                    .entry(target_idx)
                    .and_modify(|e| e.1 = new_implied)
                    .or_insert((old_implied, new_implied));

                self.graph[target_idx].implied_prob = new_implied;

                debug!(
                    target = %self.graph[target_idx].market_id,
                    old_implied,
                    new_implied,
                    actual_change,
                    depth = task.depth + 1,
                    "propagate: implied_prob updated"
                );

                queue.push_back(PropagationTask {
                    node_idx: target_idx,
                    delta: actual_change,
                    depth: task.depth + 1,
                });
            }
        }

        // ── Build result ──────────────────────────────────────────────────────
        let node_changes: Vec<NodeChange> = node_probs
            .iter()
            .filter_map(|(&idx, &(old, new))| {
                self.graph.node_weight(idx).map(|n| NodeChange {
                    market_id: n.market_id.clone(),
                    old_implied_prob: old,
                    new_implied_prob: new,
                })
            })
            .collect();

        let updated_node_ids: Vec<String> =
            node_changes.iter().map(|c| c.market_id.clone()).collect();

        if depth_limited {
            warn!(
                from = ?from_market_id,
                iterations,
                max_depth = self.config.max_depth,
                "propagate: depth limit reached — some downstream nodes not updated"
            );
        } else {
            info!(
                from = ?from_market_id,
                nodes_updated = node_changes.len(),
                iterations,
                "propagate: converged"
            );
        }

        Ok(PropagationResult {
            nodes_updated: node_changes.len(),
            iterations,
            converged: !depth_limited,
            updated_node_ids,
            node_changes,
        })
    }

    // ── Query methods ─────────────────────────────────────────────────────────

    /// Graph-implied probability for a market.
    #[must_use = "returns implied probability without modifying the graph"]
    pub fn get_implied_prob(&self, market_id: &str) -> Result<f64, GraphError> {
        Ok(self.get_market(market_id)?.implied_prob)
    }

    /// `current_prob − implied_prob` for a market.
    ///
    /// Positive → market is pricing *above* what the graph suggests (SELL signal).
    /// Negative → market is pricing *below* what the graph suggests (BUY signal).
    #[must_use = "returns deviation without modifying the graph"]
    pub fn get_deviation(&self, market_id: &str) -> Result<f64, GraphError> {
        Ok(self.get_market(market_id)?.deviation())
    }

    /// All upstream nodes and their effective edge weights into `market_id`.
    ///
    /// Represents which markets are currently influencing this market's
    /// `implied_prob`.
    #[must_use = "returns influencing nodes without modifying the graph"]
    pub fn get_influencing_nodes(
        &self,
        market_id: &str,
    ) -> Result<Vec<(String, f64)>, GraphError> {
        let idx = self.node_idx(market_id)?;
        Ok(self
            .graph
            .edges_directed(idx, Direction::Incoming)
            .map(|e| {
                let id = self.graph[e.source()].market_id.clone();
                (id, e.weight().effective_weight())
            })
            .collect())
    }

    /// All downstream nodes and their effective edge weights from `market_id`.
    ///
    /// These are the markets that would receive propagated updates when
    /// `market_id`'s probability changes.
    #[must_use = "returns dependent nodes without modifying the graph"]
    pub fn get_dependent_nodes(
        &self,
        market_id: &str,
    ) -> Result<Vec<(String, f64)>, GraphError> {
        let idx = self.node_idx(market_id)?;
        Ok(self
            .graph
            .edges_directed(idx, Direction::Outgoing)
            .map(|e| {
                let id = self.graph[e.target()].market_id.clone();
                (id, e.weight().effective_weight())
            })
            .collect())
    }

    /// All markets sorted by `|deviation|` descending, truncated to `limit`.
    ///
    /// Use this to find the most mispriced markets in the graph — prime
    /// candidates for trade signals.
    #[must_use = "returns the deviation ranking without modifying the graph"]
    pub fn top_deviations(&self, limit: usize) -> Vec<(&MarketNode, f64)> {
        let mut pairs: Vec<(&MarketNode, f64)> = self
            .index_map
            .values()
            .map(|&idx| {
                let n = &self.graph[idx];
                (n, n.deviation().abs())
            })
            .collect();
        pairs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        pairs.truncate(limit);
        pairs
    }

    /// All markets that carry any of the given tags.
    #[must_use = "returns matching nodes without modifying the graph"]
    pub fn markets_by_tag(&self, tag: &str) -> Vec<&MarketNode> {
        self.index_map
            .values()
            .filter_map(|&idx| {
                let n = &self.graph[idx];
                n.tags.iter().any(|t| t == tag).then_some(n)
            })
            .collect()
    }

    // ── Serialisation ─────────────────────────────────────────────────────────

    /// Serialise the full graph state to a pretty-printed JSON string.
    #[must_use = "returns JSON string without modifying the graph"]
    pub fn to_json(&self) -> Result<String, GraphError> {
        Ok(serde_json::to_string_pretty(&self.to_snapshot())?)
    }

    /// Deserialise from a JSON string produced by `to_json()`.
    pub fn from_json(json: &str) -> Result<Self, GraphError> {
        let snapshot: GraphSnapshot = serde_json::from_str(json)?;
        Self::from_snapshot(snapshot)
    }

    /// Export the full graph as a portable [`GraphSnapshot`].
    #[must_use = "returns a snapshot without modifying the graph"]
    pub fn to_snapshot(&self) -> GraphSnapshot {
        let nodes = self
            .index_map
            .values()
            .map(|&idx| self.graph[idx].clone())
            .collect();

        let edges = self
            .graph
            .edge_references()
            .map(|e: petgraph::stable_graph::EdgeReference<'_, MarketEdge>| e.weight().clone())
            .collect();

        GraphSnapshot {
            nodes,
            edges,
            config: self.config.clone(),
        }
    }

    /// Rebuild an engine from a [`GraphSnapshot`].
    ///
    /// All `implied_prob` values stored in the snapshot are preserved.
    pub fn from_snapshot(snapshot: GraphSnapshot) -> Result<Self, GraphError> {
        let mut engine = Self::with_config(snapshot.config);
        for node in snapshot.nodes {
            engine.add_market(node);
        }
        for edge in snapshot.edges {
            // Use add_dependency so indices are built correctly.
            // EdgeType is Copy — no .clone() needed.
            engine.add_dependency(
                &edge.from_id,
                &edge.to_id,
                edge.weight,
                edge.confidence,
                edge.edge_type,
            )?;
            // Restore edge metadata (created_at, decay) that add_dependency would overwrite
            if let (Some(&fi), Some(&ti)) = (
                engine.index_map.get(&edge.from_id),
                engine.index_map.get(&edge.to_id),
            ) {
                if let Some(eidx) = engine.graph.find_edge(fi, ti) {
                    engine.graph[eidx].created_at = edge.created_at;
                    engine.graph[eidx].decay_half_life_days = edge.decay_half_life_days;
                }
            }
        }
        Ok(engine)
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    fn node_idx(&self, market_id: &str) -> Result<NodeIndex, GraphError> {
        self.index_map
            .get(market_id)
            .copied()
            .ok_or_else(|| GraphError::MarketNotFound(market_id.to_string()))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;

    /// Tolerance for floating-point comparisons in tests.
    const EPS: f64 = 1e-6;
    /// Looser tolerance for propagation results (algorithm is approximate).
    const PROP_EPS: f64 = 1e-4;

    fn engine() -> MarketGraphEngine {
        MarketGraphEngine::new()
    }

    fn node(id: &str, prob: f64) -> MarketNode {
        MarketNode::new(id, id, prob, None, None)
    }

    // ── Basic node operations ────────────────────────────────────────────────

    #[test]
    fn test_add_and_get_market() {
        let mut e = engine();
        e.add_market(node("trump_win", 0.61));

        let n = e.get_market("trump_win").unwrap();
        assert_abs_diff_eq!(n.current_prob, 0.61, epsilon = EPS);
        assert_abs_diff_eq!(n.implied_prob, 0.61, epsilon = EPS);
        assert_eq!(n.market_id, "trump_win");
        assert_eq!(e.node_count(), 1);
    }

    #[test]
    fn test_add_market_upsert_preserves_implied_prob() {
        let mut e = engine();
        e.add_market(node("a", 0.50));

        // Manually set implied_prob (simulating propagation has run)
        e.get_market_mut("a").unwrap().implied_prob = 0.45;

        // Re-insert with updated current_prob
        let mut updated = node("a", 0.55);
        updated.title = "Updated".to_string();
        e.add_market(updated);

        let n = e.get_market("a").unwrap();
        // implied_prob must be preserved across an upsert
        assert_abs_diff_eq!(n.implied_prob, 0.45, epsilon = EPS);
        // current_prob updated
        assert_abs_diff_eq!(n.current_prob, 0.55, epsilon = EPS);
        assert_eq!(e.node_count(), 1); // no duplicate
    }

    #[test]
    fn test_remove_market() {
        let mut e = engine();
        e.add_market(node("a", 0.5));
        assert_eq!(e.node_count(), 1);
        e.remove_market("a").unwrap();
        assert_eq!(e.node_count(), 0);
        assert!(e.get_market("a").is_err());
    }

    #[test]
    fn test_market_not_found_error() {
        let e = engine();
        let err = e.get_market("nonexistent").unwrap_err();
        assert!(matches!(err, GraphError::MarketNotFound(_)));
    }

    // ── Edge operations ──────────────────────────────────────────────────────

    #[test]
    fn test_add_dependency_and_query_neighbors() {
        let mut e = engine();
        e.add_market(node("trump_win", 0.61));
        e.add_market(node("gop_senate", 0.55));
        e.add_dependency("trump_win", "gop_senate", 0.42, 0.85, EdgeType::Statistical)
            .unwrap();

        assert_eq!(e.edge_count(), 1);

        let influencing = e.get_influencing_nodes("gop_senate").unwrap();
        assert_eq!(influencing.len(), 1);
        assert_eq!(influencing[0].0, "trump_win");
        // effective_weight ≈ 0.42 × 0.85 = 0.357
        assert_abs_diff_eq!(influencing[0].1, 0.42 * 0.85, epsilon = EPS);

        let dependents = e.get_dependent_nodes("trump_win").unwrap();
        assert_eq!(dependents.len(), 1);
        assert_eq!(dependents[0].0, "gop_senate");
    }

    #[test]
    fn test_duplicate_edge_is_updated_not_duplicated() {
        let mut e = engine();
        e.add_market(node("a", 0.5));
        e.add_market(node("b", 0.5));
        e.add_dependency("a", "b", 0.3, 0.7, EdgeType::Causal).unwrap();
        e.add_dependency("a", "b", 0.6, 0.9, EdgeType::Statistical).unwrap();

        assert_eq!(e.edge_count(), 1); // still one edge
        let deps = e.get_dependent_nodes("a").unwrap();
        // effective_weight should reflect the updated values
        assert_abs_diff_eq!(deps[0].1, 0.6 * 0.9, epsilon = EPS);
    }

    #[test]
    fn test_remove_dependency() {
        let mut e = engine();
        e.add_market(node("a", 0.5));
        e.add_market(node("b", 0.5));
        e.add_dependency("a", "b", 0.5, 1.0, EdgeType::Manual).unwrap();
        assert_eq!(e.edge_count(), 1);
        e.remove_dependency("a", "b").unwrap();
        assert_eq!(e.edge_count(), 0);
    }

    // ── Propagation ──────────────────────────────────────────────────────────

    /// A → B → C chain.  Update A, verify the signal propagates through B to C.
    ///
    /// ```text
    /// A(0.50) --w=0.6--> B(0.50) --w=0.6--> C(0.50)
    /// Update A to 0.70 (delta = +0.20)
    ///
    /// After one iteration:
    ///   B_implied += 0.6 × 0.20 × 0.85 = 0.1020
    ///   C_implied += 0.6 × 0.1020 × 0.85 = 0.05202  (second iteration)
    /// ```
    #[test]
    fn test_simple_chain_propagation() {
        let mut e = engine();
        e.add_market(node("a", 0.50));
        e.add_market(node("b", 0.50));
        e.add_market(node("c", 0.50));
        e.add_dependency("a", "b", 0.6, 1.0, EdgeType::Causal).unwrap();
        e.add_dependency("b", "c", 0.6, 1.0, EdgeType::Causal).unwrap();

        let result = e.update_and_propagate("a", 0.70, UpdateSource::Api).unwrap();

        // Both B and C should have been updated
        assert!(result.nodes_updated >= 2, "expected at least B and C updated");
        assert!(result.converged);

        // updated_node_ids should include both b and c
        assert!(result.updated_node_ids.contains(&"b".to_string()));
        assert!(result.updated_node_ids.contains(&"c".to_string()));

        let b_implied = e.get_implied_prob("b").unwrap();
        let c_implied = e.get_implied_prob("c").unwrap();

        // B should be above its initial 0.50
        assert!(
            b_implied > 0.50,
            "B implied_prob should rise; got {b_implied:.4}"
        );
        // C should also be above 0.50
        assert!(
            c_implied > 0.50,
            "C implied_prob should rise; got {c_implied:.4}"
        );
        // B should be closer to A's change than C (signal attenuates with distance)
        assert!(b_implied > c_implied, "signal must attenuate A→B→C");

        // Approximate manual calculation: B ≈ 0.50 + 0.6×0.20×0.85 = 0.602
        assert_abs_diff_eq!(b_implied, 0.50 + 0.6 * 0.20 * 0.85, epsilon = PROP_EPS);
    }

    /// A → B with a negative weight.  When A rises, B's implied_prob should fall.
    #[test]
    fn test_negative_correlation_propagation() {
        let mut e = engine();
        e.add_market(node("fed_hike", 0.50));
        e.add_market(node("btc_ath", 0.40));
        // Fed hike is bad for crypto: weight = -0.5
        e.add_dependency("fed_hike", "btc_ath", -0.5, 1.0, EdgeType::Statistical)
            .unwrap();

        e.update_and_propagate("fed_hike", 0.70, UpdateSource::News).unwrap();

        let btc_implied = e.get_implied_prob("btc_ath").unwrap();
        // BTC ATH should be revised downward
        assert!(
            btc_implied < 0.40,
            "BTC implied_prob should fall; got {btc_implied:.4}"
        );

        // Approximate: 0.40 + (-0.5) × 0.20 × 0.85 = 0.40 − 0.085 = 0.315
        assert_abs_diff_eq!(btc_implied, 0.40 - 0.5 * 0.20 * 0.85, epsilon = PROP_EPS);
    }

    /// A → B → A cycle must not loop forever.
    ///
    /// With `max_depth = 8` the BFS traverses at most 8 hops; the damping
    /// factor ensures the signal falls below `convergence_threshold` well
    /// before that, so the queue empties naturally and all values stay in [0,1].
    #[test]
    fn test_cycle_does_not_hang() {
        let mut e = MarketGraphEngine::with_config(PropagationConfig {
            max_depth: 8,
            ..PropagationConfig::default()
        });
        e.add_market(node("a", 0.50));
        e.add_market(node("b", 0.50));
        e.add_dependency("a", "b", 0.5, 1.0, EdgeType::Statistical).unwrap();
        e.add_dependency("b", "a", 0.5, 1.0, EdgeType::Statistical).unwrap();

        // Must complete without hanging
        let result = e
            .update_and_propagate("a", 0.70, UpdateSource::Api)
            .unwrap();

        // Queue must drain within the depth bound
        assert!(result.iterations <= 8);

        // Both nodes' implied_probs should have moved
        let a_implied = e.get_implied_prob("a").unwrap();
        let b_implied = e.get_implied_prob("b").unwrap();

        // Signal is damped — values must remain in [0, 1]
        assert!(
            (0.0..=1.0).contains(&a_implied),
            "a implied out of bounds: {a_implied}"
        );
        assert!(
            (0.0..=1.0).contains(&b_implied),
            "b implied out of bounds: {b_implied}"
        );
    }

    /// Verify deviation = current_prob − implied_prob.
    #[test]
    fn test_deviation_calculation() {
        let mut e = engine();
        e.add_market(node("a", 0.50));
        e.add_market(node("b", 0.50));
        e.add_dependency("a", "b", 0.8, 1.0, EdgeType::Causal).unwrap();

        // A jumps from 0.50 to 0.70; propagate to B
        e.update_and_propagate("a", 0.70, UpdateSource::Api).unwrap();

        // B's market price is still 0.50 (no external update)
        // B's implied_prob has risen due to propagation
        let dev = e.get_deviation("b").unwrap();
        // dev should be negative: B's market price (0.50) < implied_prob (> 0.50)
        assert!(dev < 0.0, "expected negative deviation; got {dev:.4}");
    }

    /// Serialise to JSON and deserialise back; verify the round-trip is lossless.
    #[test]
    fn test_serialization_round_trip() {
        let mut e = engine();
        e.add_market(
            MarketNode::new("trump_win", "Trump wins 2028", 0.61, Some(100_000.0), Some("2028-11-03".to_string()))
                .with_tags(["politics", "us-election-2028"]),
        );
        e.add_market(node("gop_senate", 0.55));
        e.add_dependency("trump_win", "gop_senate", 0.42, 0.85, EdgeType::Statistical)
            .unwrap();
        e.update_and_propagate("trump_win", 0.71, UpdateSource::Api).unwrap();

        let json = e.to_json().unwrap();
        let e2 = MarketGraphEngine::from_json(&json).unwrap();

        assert_eq!(e2.node_count(), 2);
        assert_eq!(e2.edge_count(), 1);

        let n1 = e2.get_market("trump_win").unwrap();
        assert_abs_diff_eq!(n1.current_prob, 0.71, epsilon = EPS);
        assert_eq!(n1.tags, vec!["politics", "us-election-2028"]);

        // implied_prob of gop_senate should be preserved across round-trip
        let orig_implied = e.get_implied_prob("gop_senate").unwrap();
        let rt_implied   = e2.get_implied_prob("gop_senate").unwrap();
        assert_abs_diff_eq!(orig_implied, rt_implied, epsilon = EPS);
    }

    /// Signal must attenuate over a long chain A→B→C→D→E.
    #[test]
    fn test_damping_attenuation_over_long_chain() {
        let mut e = engine();
        let chain = ["a", "b", "c", "d", "e"];
        for id in chain {
            e.add_market(node(id, 0.50));
        }
        for w in chain.windows(2) {
            e.add_dependency(w[0], w[1], 1.0, 1.0, EdgeType::Causal).unwrap();
        }

        e.update_and_propagate("a", 0.70, UpdateSource::Api).unwrap();

        let probs: Vec<f64> = chain
            .iter()
            .map(|id| e.get_implied_prob(id).unwrap())
            .collect();

        // Implied probs should form a strictly decreasing sequence after A
        // (A itself = 0.70 after reset; B < A; C < B; etc.)
        for pair in probs[1..].windows(2) {
            assert!(
                pair[0] >= pair[1],
                "signal did not attenuate: {:.4} < {:.4}",
                pair[0],
                pair[1]
            );
        }
    }

    /// Batch update two roots, then verify propagation covers their shared child.
    #[test]
    fn test_batch_update_and_propagate() {
        let mut e = engine();
        // Two independent upstream markets feeding one shared downstream market
        e.add_market(node("macro_risk", 0.50));
        e.add_market(node("politics_risk", 0.50));
        e.add_market(node("sp500_down", 0.30));
        e.add_dependency("macro_risk", "sp500_down", 0.6, 1.0, EdgeType::Causal).unwrap();
        e.add_dependency("politics_risk", "sp500_down", 0.4, 1.0, EdgeType::Causal).unwrap();

        let result = e
            .batch_update_and_propagate(&[
                ("macro_risk", 0.70, UpdateSource::Api),
                ("politics_risk", 0.65, UpdateSource::News),
            ])
            .unwrap();

        assert!(result.nodes_updated >= 1);

        let sp_implied = e.get_implied_prob("sp500_down").unwrap();
        // Both upstream markets moved up → sp500_down implied should rise above 0.30
        assert!(
            sp_implied > 0.30,
            "sp500_down implied_prob should rise; got {sp_implied:.4}"
        );
    }

    /// Verify top_deviations returns sorted results.
    #[test]
    fn test_top_deviations_sorted() {
        let mut e = engine();
        for (id, prob) in [("a", 0.50), ("b", 0.50), ("c", 0.50)] {
            e.add_market(node(id, prob));
        }
        e.add_dependency("a", "b", 0.8, 1.0, EdgeType::Causal).unwrap();
        e.add_dependency("a", "c", 0.3, 1.0, EdgeType::Causal).unwrap();

        e.update_and_propagate("a", 0.80, UpdateSource::Api).unwrap();

        let top = e.top_deviations(3);
        // Deviations must be non-increasing
        for pair in top.windows(2) {
            assert!(
                pair[0].1 >= pair[1].1,
                "top_deviations not sorted: {:.4} < {:.4}",
                pair[0].1,
                pair[1].1
            );
        }
        // B has a stronger edge than C, so B's deviation should be largest
        if top.len() >= 2 {
            let ids: Vec<&str> = top.iter().map(|(n, _)| n.market_id.as_str()).collect();
            assert_eq!(ids[0], "b", "B should have largest deviation");
        }
    }

    /// Filter by tag returns only matching nodes.
    #[test]
    fn test_markets_by_tag() {
        let mut e = engine();
        e.add_market(MarketNode::new("trump", "Trump wins", 0.6, None, None).with_tags(["politics"]));
        e.add_market(MarketNode::new("btc", "BTC 100k", 0.4, None, None).with_tags(["crypto"]));
        e.add_market(
            MarketNode::new("eth", "ETH 5k", 0.35, None, None).with_tags(["crypto"]),
        );

        let crypto = e.markets_by_tag("crypto");
        assert_eq!(crypto.len(), 2);

        let politics = e.markets_by_tag("politics");
        assert_eq!(politics.len(), 1);
        assert_eq!(politics[0].market_id, "trump");

        let empty = e.markets_by_tag("sports");
        assert!(empty.is_empty());
    }

    /// Invalid probability inputs must return an error.
    #[test]
    fn test_invalid_probability_rejected() {
        let mut e = engine();
        e.add_market(node("a", 0.5));
        assert!(e.update_market_prob("a", 1.5, UpdateSource::Api).is_err());
        assert!(e.update_market_prob("a", -0.1, UpdateSource::Api).is_err());
    }

    /// Invalid weight inputs must return an error.
    #[test]
    fn test_invalid_weight_rejected() {
        let mut e = engine();
        e.add_market(node("a", 0.5));
        e.add_market(node("b", 0.5));
        assert!(e.add_dependency("a", "b", 1.5, 0.8, EdgeType::Causal).is_err());
        assert!(e.add_dependency("a", "b", -2.0, 0.8, EdgeType::Causal).is_err());
    }

    /// PropagationResult.updated_node_ids covers all hops in a chain.
    #[test]
    fn test_propagation_result_includes_all_hops() {
        let mut e = engine();
        e.add_market(node("a", 0.50));
        e.add_market(node("b", 0.50));
        e.add_market(node("c", 0.50));
        e.add_dependency("a", "b", 0.8, 1.0, EdgeType::Causal).unwrap();
        e.add_dependency("b", "c", 0.8, 1.0, EdgeType::Causal).unwrap();

        let result = e.update_and_propagate("a", 0.70, UpdateSource::Api).unwrap();

        // Both b and c must appear — not just direct neighbour b
        assert!(
            result.updated_node_ids.contains(&"b".to_string()),
            "b missing from updated_node_ids"
        );
        assert!(
            result.updated_node_ids.contains(&"c".to_string()),
            "c missing from updated_node_ids (multi-hop not reported)"
        );
        // nodes_updated is distinct count
        assert_eq!(result.nodes_updated, result.updated_node_ids.len());
    }
}
