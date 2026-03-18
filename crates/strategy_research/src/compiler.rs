// crates/strategy_research/src/compiler.rs
//
// StrategyCompiler — converts a Hypothesis into an executable GeneratedStrategy.
// GeneratedStrategy::evaluate() is the hot path called by the backtest runner.

use crate::types::{
    DirectionRule, GeneratedStrategy, Hypothesis, LogicalOp, MarketSnapshot, SizingRule,
};
use common::TradeDirection;

/// Compiles `Hypothesis` values into `GeneratedStrategy` values.
///
/// Currently a stateless transform; kept as a struct for future extension
/// (e.g., caching compiled rule closures).
pub struct StrategyCompiler;

impl StrategyCompiler {
    /// Compile one hypothesis into an executable strategy.
    pub fn compile(hypothesis: Hypothesis) -> GeneratedStrategy {
        let name = format!(
            "{}_{}",
            hypothesis.template_name,
            &hypothesis.id.to_string()[..8]
        );
        GeneratedStrategy {
            id: hypothesis.id,
            hypothesis,
            name,
        }
    }
}

// ---------------------------------------------------------------------------
// Strategy evaluation — called once per market per tick during backtesting
// ---------------------------------------------------------------------------

impl GeneratedStrategy {
    /// Evaluate the strategy against a market snapshot.
    ///
    /// Returns `Some((direction, position_fraction))` when all conditions fire;
    /// `None` when conditions are not met or sizing resolves to zero.
    pub fn evaluate(&self, snapshot: &MarketSnapshot) -> Option<(TradeDirection, f64)> {
        if !self.conditions_met(snapshot) {
            return None;
        }
        let direction = self.resolve_direction(snapshot);
        let fraction = self.resolve_sizing(snapshot);
        if fraction <= 0.0 {
            return None;
        }
        Some((direction, fraction))
    }

    // ── Private helpers ───────────────────────────────────────────────────

    fn conditions_met(&self, snapshot: &MarketSnapshot) -> bool {
        if self.hypothesis.conditions.is_empty() {
            return false;
        }
        match self.hypothesis.logical_op {
            LogicalOp::And => self
                .hypothesis
                .conditions
                .iter()
                .all(|c| c.evaluate(snapshot)),
            LogicalOp::Or => self
                .hypothesis
                .conditions
                .iter()
                .any(|c| c.evaluate(snapshot)),
        }
    }

    fn resolve_direction(&self, snapshot: &MarketSnapshot) -> TradeDirection {
        match self.hypothesis.direction_rule {
            DirectionRule::AlwaysBuy => TradeDirection::Buy,
            DirectionRule::AlwaysSell => TradeDirection::Sell,
            DirectionRule::FollowPosterior => {
                // Positive edge → model above market → BUY.
                if snapshot.posterior_edge >= 0.0 {
                    TradeDirection::Buy
                } else {
                    TradeDirection::Sell
                }
            }
            DirectionRule::Contrarian => {
                // Fade the posterior signal.
                if snapshot.posterior_edge >= 0.0 {
                    TradeDirection::Sell
                } else {
                    TradeDirection::Buy
                }
            }
            DirectionRule::FollowMomentum => {
                if snapshot.temporal_z_score >= 0.0 {
                    TradeDirection::Buy
                } else {
                    TradeDirection::Sell
                }
            }
        }
    }

    fn resolve_sizing(&self, snapshot: &MarketSnapshot) -> f64 {
        match &self.hypothesis.sizing_rule {
            SizingRule::Fixed(f) => *f,
            SizingRule::Kelly => {
                kelly_fraction(snapshot.posterior_prob, snapshot.market_prob, 0.25, 0.05)
            }
            SizingRule::HalfKelly => {
                kelly_fraction(snapshot.posterior_prob, snapshot.market_prob, 0.125, 0.025)
            }
            SizingRule::ScaleWithConfidence { max_fraction } => {
                // Scale fraction linearly with the absolute edge, capped at max_fraction.
                let confidence = snapshot.posterior_edge.abs().min(1.0);
                (confidence * max_fraction).clamp(0.0, *max_fraction)
            }
        }
    }
}

/// Fractional Kelly position size.
///
/// `p` = model probability, `m` = market probability.
/// `scale` = Kelly fraction (0.25 = quarter-Kelly).
/// `cap` = hard maximum fraction of capital.
fn kelly_fraction(p: f64, m: f64, scale: f64, cap: f64) -> f64 {
    let p = p.clamp(0.01, 0.99);
    let m = m.clamp(0.01, 0.99);
    let f = if p > m {
        (p - m) / (1.0 - m)
    } else {
        (m - p) / m
    };
    (f * scale).clamp(0.0, cap)
}
