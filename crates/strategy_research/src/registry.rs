// crates/strategy_research/src/registry.rs
//
// StrategyRegistry — stores DiscoveredStrategy values that passed evaluation.
//
// Capacity enforcement: when full, a new strategy is accepted only if its
// Sharpe ratio exceeds the worst entry's Sharpe.  The worst entry is evicted.

use crate::{
    config::ResearchConfig,
    types::{DiscoveredStrategy, Hypothesis, StrategyMetrics},
};
use chrono::Utc;
use std::collections::HashMap;

/// In-memory store of promoted strategies, keyed by UUID string.
pub struct StrategyRegistry {
    strategies: HashMap<String, DiscoveredStrategy>,
    capacity: usize,
    pub min_sharpe: f64,
    pub max_drawdown: f64,
    pub min_trades: usize,
}

impl StrategyRegistry {
    pub fn new(config: &ResearchConfig) -> Self {
        Self {
            strategies: HashMap::new(),
            capacity: config.registry_capacity,
            min_sharpe: config.min_sharpe,
            max_drawdown: config.max_drawdown,
            min_trades: config.min_trades,
        }
    }

    // ── Promotion criteria ────────────────────────────────────────────────

    /// Return `true` when `metrics` satisfies all promotion thresholds.
    pub fn meets_criteria(&self, metrics: &StrategyMetrics) -> bool {
        metrics.sharpe_ratio >= self.min_sharpe
            && metrics.max_drawdown <= self.max_drawdown
            && metrics.trade_count >= self.min_trades
    }

    /// Static version for callers that have their own criteria values.
    pub fn meets_promotion_criteria(
        metrics: &StrategyMetrics,
        config: &ResearchConfig,
    ) -> bool {
        metrics.sharpe_ratio >= config.min_sharpe
            && metrics.max_drawdown <= config.max_drawdown
            && metrics.trade_count >= config.min_trades
    }

    // ── Insertion ─────────────────────────────────────────────────────────

    /// Attempt to insert a strategy.
    ///
    /// Returns the `DiscoveredStrategy` if it was accepted (new or better than
    /// current worst), `None` if rejected due to capacity/quality constraints.
    pub fn try_insert(
        &mut self,
        hypothesis: Hypothesis,
        metrics: StrategyMetrics,
    ) -> Option<DiscoveredStrategy> {
        let id = hypothesis.id.to_string();

        // Skip duplicates.
        if self.strategies.contains_key(&id) {
            return None;
        }

        let candidate = DiscoveredStrategy {
            id: id.clone(),
            hypothesis,
            metrics,
            promoted_at: Utc::now(),
        };

        // Evict worst entry if at capacity.
        if self.strategies.len() >= self.capacity {
            let worst_id = self
                .strategies
                .values()
                .min_by(|a, b| {
                    a.metrics
                        .sharpe_ratio
                        .partial_cmp(&b.metrics.sharpe_ratio)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|s| s.id.clone());

            match worst_id {
                Some(wid) => {
                    let worst_sharpe = self.strategies[&wid].metrics.sharpe_ratio;
                    if candidate.metrics.sharpe_ratio <= worst_sharpe {
                        return None; // candidate is no better than the worst — reject
                    }
                    self.strategies.remove(&wid);
                }
                None => return None, // at capacity but no worst entry found — reject
            }
        }

        self.strategies.insert(id, candidate.clone());
        Some(candidate)
    }

    // ── Queries ───────────────────────────────────────────────────────────

    /// Return up to `limit` strategies sorted by Sharpe ratio (descending).
    pub fn top_strategies(&self, limit: usize) -> Vec<&DiscoveredStrategy> {
        let mut all: Vec<&DiscoveredStrategy> = self.strategies.values().collect();
        all.sort_by(|a, b| {
            b.metrics
                .sharpe_ratio
                .partial_cmp(&a.metrics.sharpe_ratio)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        all.into_iter().take(limit).collect()
    }

    /// All strategies, unsorted.
    pub fn all(&self) -> impl Iterator<Item = &DiscoveredStrategy> {
        self.strategies.values()
    }

    pub fn len(&self) -> usize {
        self.strategies.len()
    }

    pub fn is_empty(&self) -> bool {
        self.strategies.is_empty()
    }

    /// Look up a strategy by its UUID string.
    pub fn get(&self, id: &str) -> Option<&DiscoveredStrategy> {
        self.strategies.get(id)
    }
}
