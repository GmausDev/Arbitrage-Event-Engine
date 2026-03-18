// crates/strategy_research/src/templates.rs
//
// Reusable strategy archetypes.  Each template defines the shape of a class
// of hypotheses; the HypothesisGenerator samples concrete thresholds within
// the ranges appropriate for each feature.

use crate::types::{DirectionRule, LogicalOp, SignalFeature, SizingRule, StrategyTemplate};

/// Return the full catalogue of strategy templates.
pub fn all_templates() -> Vec<StrategyTemplate> {
    vec![
        // ── Single-signal strategies ──────────────────────────────────────
        StrategyTemplate {
            name: "BayesianMomentum",
            description: "Trade when the Bayesian posterior diverges from the market price.",
            features: vec![SignalFeature::PosteriorEdge],
            condition_count: 1,
            logical_op: LogicalOp::And,
            direction_rule: DirectionRule::FollowPosterior,
            sizing_rule: SizingRule::Kelly,
        },
        StrategyTemplate {
            name: "GraphArb",
            description: "Trade on graph-implied vs market-price divergence.",
            features: vec![SignalFeature::GraphArbitrageEdge],
            condition_count: 1,
            logical_op: LogicalOp::And,
            direction_rule: DirectionRule::FollowPosterior,
            sizing_rule: SizingRule::Fixed(0.02),
        },
        StrategyTemplate {
            name: "TrendFollower",
            description: "Follow momentum when the rolling z-score exceeds a threshold.",
            features: vec![SignalFeature::TemporalMomentum],
            condition_count: 1,
            logical_op: LogicalOp::And,
            direction_rule: DirectionRule::FollowMomentum,
            sizing_rule: SizingRule::HalfKelly,
        },
        StrategyTemplate {
            name: "ShockTrader",
            description: "Enter on detected information shocks.",
            features: vec![SignalFeature::ShockSignal],
            condition_count: 1,
            logical_op: LogicalOp::And,
            direction_rule: DirectionRule::FollowPosterior,
            sizing_rule: SizingRule::ScaleWithConfidence { max_fraction: 0.05 },
        },
        StrategyTemplate {
            name: "VolatilityGated",
            description: "Take Bayesian signals only when volatility is elevated.",
            features: vec![SignalFeature::MarketVolatility, SignalFeature::PosteriorEdge],
            condition_count: 2,
            logical_op: LogicalOp::And,
            direction_rule: DirectionRule::FollowPosterior,
            sizing_rule: SizingRule::Fixed(0.02),
        },
        // ── Multi-signal confirmation strategies ──────────────────────────
        StrategyTemplate {
            name: "BayesianGraphConfirmation",
            description: "Enter only when both posterior edge and graph edge agree.",
            features: vec![SignalFeature::PosteriorEdge, SignalFeature::GraphArbitrageEdge],
            condition_count: 2,
            logical_op: LogicalOp::And,
            direction_rule: DirectionRule::FollowPosterior,
            sizing_rule: SizingRule::Kelly,
        },
        StrategyTemplate {
            name: "ShockMomentum",
            description: "Require shock AND momentum confirmation before entering.",
            features: vec![SignalFeature::ShockSignal, SignalFeature::TemporalMomentum],
            condition_count: 2,
            logical_op: LogicalOp::And,
            direction_rule: DirectionRule::FollowMomentum,
            sizing_rule: SizingRule::HalfKelly,
        },
        StrategyTemplate {
            name: "TripleConfirmation",
            description: "Enter only when posterior, graph, and momentum all agree.",
            features: vec![
                SignalFeature::PosteriorEdge,
                SignalFeature::GraphArbitrageEdge,
                SignalFeature::TemporalMomentum,
            ],
            condition_count: 3,
            logical_op: LogicalOp::And,
            direction_rule: DirectionRule::FollowPosterior,
            sizing_rule: SizingRule::Kelly,
        },
        // ── Contrarian / mean-reversion strategies ────────────────────────
        StrategyTemplate {
            name: "MeanReversionContrarian",
            description: "Fade momentum when the z-score is extreme.",
            features: vec![SignalFeature::TemporalMomentum],
            condition_count: 1,
            logical_op: LogicalOp::And,
            direction_rule: DirectionRule::Contrarian,
            sizing_rule: SizingRule::Fixed(0.015),
        },
        StrategyTemplate {
            name: "PostShockReversal",
            description: "Bet against the shock direction when volatility spikes.",
            features: vec![SignalFeature::ShockSignal, SignalFeature::MarketVolatility],
            condition_count: 2,
            logical_op: LogicalOp::Or,
            direction_rule: DirectionRule::Contrarian,
            sizing_rule: SizingRule::Fixed(0.02),
        },
        StrategyTemplate {
            name: "BayesianContrarian",
            description: "Fade large posterior deviations as overreactions.",
            features: vec![SignalFeature::PosteriorEdge],
            condition_count: 1,
            logical_op: LogicalOp::And,
            direction_rule: DirectionRule::Contrarian,
            sizing_rule: SizingRule::HalfKelly,
        },
        // ── Or-logic / opportunistic strategies ───────────────────────────
        StrategyTemplate {
            name: "OpportunisticEntry",
            description: "Enter on any detectable edge: posterior OR graph.",
            features: vec![SignalFeature::PosteriorEdge, SignalFeature::GraphArbitrageEdge],
            condition_count: 2,
            logical_op: LogicalOp::Or,
            direction_rule: DirectionRule::FollowPosterior,
            sizing_rule: SizingRule::HalfKelly,
        },
        StrategyTemplate {
            name: "AnySignalBuy",
            description: "Buy on any one of: posterior edge, shock, or momentum.",
            features: vec![
                SignalFeature::PosteriorEdge,
                SignalFeature::ShockSignal,
                SignalFeature::TemporalMomentum,
            ],
            condition_count: 3,
            logical_op: LogicalOp::Or,
            direction_rule: DirectionRule::AlwaysBuy,
            sizing_rule: SizingRule::Fixed(0.01),
        },
    ]
}
