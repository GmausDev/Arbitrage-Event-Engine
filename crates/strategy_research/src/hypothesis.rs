// crates/strategy_research/src/hypothesis.rs
//
// HypothesisGenerator — creates candidate trading rules by randomly sampling
// thresholds over a catalogue of StrategyTemplates.

use crate::types::{
    ComparisonOp, DirectionRule, Hypothesis, LogicalOp, SignalCondition, SignalFeature,
    SizingRule, StrategyTemplate,
};
use chrono::Utc;
use rand::{rngs::SmallRng, Rng, SeedableRng};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Per-feature threshold sampling ranges
// ---------------------------------------------------------------------------

/// `(lo, hi)` inclusive threshold range for each feature.
///
/// Chosen to be realistic relative to the synthetic backtest data:
/// - vol ≈ 0.015/tick → posterior noise σ ≈ 0.025 → edges typically ±0.05
/// - momentum z-score is standard-normal-ish over a 20-tick window
/// - shock magnitude is in [0, 1] by construction
/// - volatility (rolling stddev of price) ≈ 0.015 baseline
fn threshold_range(feature: SignalFeature) -> (f64, f64) {
    match feature {
        SignalFeature::PosteriorEdge => (0.01, 0.25),
        SignalFeature::GraphArbitrageEdge => (0.01, 0.15),
        SignalFeature::TemporalMomentum => (0.5, 3.5),
        SignalFeature::ShockSignal => (0.10, 0.80),
        SignalFeature::MarketVolatility => (0.005, 0.04),
    }
}

/// Sample the most semantically appropriate `ComparisonOp` for a feature.
fn sample_op(feature: SignalFeature, rng: &mut SmallRng) -> ComparisonOp {
    match feature {
        // Edge signals: direction is resolved by DirectionRule, so abs-compare makes sense.
        SignalFeature::PosteriorEdge | SignalFeature::GraphArbitrageEdge => {
            ComparisonOp::AbsGreaterThan
        }
        // Momentum: usually compared in absolute terms; occasionally directional.
        SignalFeature::TemporalMomentum => {
            if rng.gen_bool(0.75) {
                ComparisonOp::AbsGreaterThan
            } else if rng.gen_bool(0.5) {
                ComparisonOp::GreaterThan
            } else {
                ComparisonOp::LessThan
            }
        }
        // Shock magnitude is always non-negative → GreaterThan is the natural op.
        SignalFeature::ShockSignal => ComparisonOp::GreaterThan,
        // Volatility: usually want "elevated" (GT) but occasionally "low vol" (LT).
        SignalFeature::MarketVolatility => {
            if rng.gen_bool(0.8) {
                ComparisonOp::GreaterThan
            } else {
                ComparisonOp::LessThan
            }
        }
    }
}

// ---------------------------------------------------------------------------
// HypothesisGenerator
// ---------------------------------------------------------------------------

/// Generates candidate `Hypothesis` values by randomly permuting templates and
/// sampling concrete parameter values.
pub struct HypothesisGenerator {
    rng: SmallRng,
    templates: Vec<StrategyTemplate>,
}

impl HypothesisGenerator {
    /// Create a new generator.
    ///
    /// `seed = Some(s)` → deterministic; `seed = None` → OS entropy.
    pub fn new(seed: Option<u64>, templates: Vec<StrategyTemplate>) -> Self {
        let rng = match seed {
            Some(s) => SmallRng::seed_from_u64(s),
            None => SmallRng::from_entropy(),
        };
        Self { rng, templates }
    }

    /// Generate `count` hypotheses.
    pub fn generate(&mut self, count: usize) -> Vec<Hypothesis> {
        let now = Utc::now();
        (0..count).map(|_| self.one(now)).collect()
    }

    fn one(&mut self, now: chrono::DateTime<Utc>) -> Hypothesis {
        let idx = self.rng.gen_range(0..self.templates.len());

        // Extract everything we need from the template before any &mut self calls.
        let features: Vec<SignalFeature> = self.templates[idx]
            .features
            .iter()
            .take(self.templates[idx].condition_count)
            .copied()
            .collect();
        let logical_op = self.templates[idx].logical_op;
        let direction_rule = self.templates[idx].direction_rule;
        let base_sizing = self.templates[idx].sizing_rule.clone();
        let template_name = self.templates[idx].name.to_string();

        let conditions = features
            .into_iter()
            .map(|f| self.sample_condition(f))
            .collect();

        let sizing_rule = self.sample_sizing(&base_sizing);

        Hypothesis {
            id: Uuid::new_v4(),
            conditions,
            logical_op,
            direction_rule,
            sizing_rule,
            template_name,
            created_at: now,
        }
    }

    fn sample_condition(&mut self, feature: SignalFeature) -> SignalCondition {
        let (lo, hi) = threshold_range(feature);
        let threshold = self.rng.gen_range(lo..=hi);
        let op = sample_op(feature, &mut self.rng);
        SignalCondition { feature, op, threshold }
    }

    /// Preserve the base sizing variant but randomise the numeric parameter.
    fn sample_sizing(&mut self, base: &SizingRule) -> SizingRule {
        match base {
            SizingRule::Fixed(_) => SizingRule::Fixed(self.rng.gen_range(0.005_f64..=0.05)),
            SizingRule::Kelly => SizingRule::Kelly,
            SizingRule::HalfKelly => SizingRule::HalfKelly,
            SizingRule::ScaleWithConfidence { .. } => SizingRule::ScaleWithConfidence {
                max_fraction: self.rng.gen_range(0.02_f64..=0.06),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Optional: additional mutators for exploration diversity
// ---------------------------------------------------------------------------

impl HypothesisGenerator {
    /// Mutate an existing hypothesis (add/remove one condition, perturb a threshold).
    ///
    /// Useful for genetic-style local search; not used in the base research loop
    /// but available for future extensions.
    pub fn mutate(&mut self, original: &Hypothesis) -> Hypothesis {
        let mut h = original.clone();
        h.id = Uuid::new_v4();
        h.created_at = Utc::now();

        let mutation_type = self.rng.gen_range(0u8..3);
        match mutation_type {
            0 => {
                // Perturb one threshold by ±25 %.
                if let Some(c) = h.conditions.first_mut() {
                    let delta = c.threshold * self.rng.gen_range(-0.25_f64..=0.25);
                    let (lo, hi) = threshold_range(c.feature);
                    c.threshold = (c.threshold + delta).clamp(lo, hi);
                }
            }
            1 => {
                // Flip logical op.
                h.logical_op = match h.logical_op {
                    LogicalOp::And => LogicalOp::Or,
                    LogicalOp::Or => LogicalOp::And,
                };
            }
            2 => {
                // Flip to a contrarian direction.
                h.direction_rule = match h.direction_rule {
                    DirectionRule::FollowPosterior => DirectionRule::Contrarian,
                    DirectionRule::Contrarian => DirectionRule::FollowPosterior,
                    DirectionRule::FollowMomentum => DirectionRule::Contrarian,
                    other => other,
                };
            }
            _ => {}
        }

        h
    }
}
