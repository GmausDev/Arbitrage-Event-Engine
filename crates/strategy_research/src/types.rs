// crates/strategy_research/src/types.rs
//
// All data structures for the automated hypothesis and testing loop.

use chrono::{DateTime, Utc};
use common::TradeDirection;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Signal feature vocabulary
// ---------------------------------------------------------------------------

/// A measurable market feature that can appear in a strategy condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SignalFeature {
    /// `posterior_prob − market_prob`: Bayesian model edge.
    PosteriorEdge,
    /// `graph_implied_prob − market_prob`: structural arbitrage edge from BFS propagation.
    GraphArbitrageEdge,
    /// Rolling z-score of per-tick price changes: momentum signal.
    TemporalMomentum,
    /// Information-shock magnitude in [0, 1].
    ShockSignal,
    /// Rolling standard deviation of market probability: volatility signal.
    MarketVolatility,
}

/// Comparison operator used in a `SignalCondition`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ComparisonOp {
    GreaterThan,
    LessThan,
    /// `|value| > threshold` — direction-agnostic magnitude check.
    AbsGreaterThan,
}

/// One evaluable predicate: `feature op threshold`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalCondition {
    pub feature: SignalFeature,
    pub op: ComparisonOp,
    pub threshold: f64,
}

impl SignalCondition {
    /// Evaluate this condition against a market snapshot.
    pub fn evaluate(&self, snapshot: &MarketSnapshot) -> bool {
        let value = match self.feature {
            SignalFeature::PosteriorEdge => snapshot.posterior_edge,
            SignalFeature::GraphArbitrageEdge => snapshot.graph_arb_edge,
            SignalFeature::TemporalMomentum => snapshot.temporal_z_score,
            SignalFeature::ShockSignal => snapshot.shock_magnitude,
            SignalFeature::MarketVolatility => snapshot.volatility,
        };
        match self.op {
            ComparisonOp::GreaterThan => value > self.threshold,
            ComparisonOp::LessThan => value < self.threshold,
            ComparisonOp::AbsGreaterThan => value.abs() > self.threshold,
        }
    }
}

// ---------------------------------------------------------------------------
// Hypothesis building blocks
// ---------------------------------------------------------------------------

/// How multiple conditions are combined into one decision predicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogicalOp {
    /// All conditions must be satisfied.
    And,
    /// At least one condition must be satisfied.
    Or,
}

/// Rule for deriving the trade direction from market state when conditions fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DirectionRule {
    AlwaysBuy,
    AlwaysSell,
    /// Buy when `posterior_edge ≥ 0` (model above market), sell otherwise.
    FollowPosterior,
    /// Opposite of `FollowPosterior` — contrarian mean-reversion bet.
    Contrarian,
    /// Buy on positive momentum z-score, sell on negative.
    FollowMomentum,
}

/// Position-sizing rule applied when strategy conditions fire.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum SizingRule {
    /// Fixed fraction of capital.
    Fixed(f64),
    /// Full Kelly criterion (25 % fractional, capped at 5 %).
    Kelly,
    /// Half-Kelly (12.5 % fractional, capped at 2.5 %).
    HalfKelly,
    /// Scale fraction linearly with `|posterior_edge|`, up to `max_fraction`.
    ScaleWithConfidence { max_fraction: f64 },
}

// ---------------------------------------------------------------------------
// Hypothesis and compiled strategy
// ---------------------------------------------------------------------------

/// A candidate trading rule: the unit of hypothesis generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hypothesis {
    pub id: Uuid,
    pub conditions: Vec<SignalCondition>,
    pub logical_op: LogicalOp,
    pub direction_rule: DirectionRule,
    pub sizing_rule: SizingRule,
    /// Name of the `StrategyTemplate` this was generated from.
    pub template_name: String,
    pub created_at: DateTime<Utc>,
}

/// A reusable archetype that defines the shape and semantics of a hypothesis class.
#[derive(Debug, Clone)]
pub struct StrategyTemplate {
    pub name: &'static str,
    pub description: &'static str,
    /// Features used to build conditions (ordered; first `condition_count` are used).
    pub features: Vec<SignalFeature>,
    /// Number of conditions drawn from `features`.
    pub condition_count: usize,
    pub logical_op: LogicalOp,
    pub direction_rule: DirectionRule,
    /// Base sizing rule — the generator may randomise the numeric parameter.
    pub sizing_rule: SizingRule,
}

/// A compiled, executable strategy ready for backtesting.
///
/// Thin wrapper around `Hypothesis` that adds a `name` and exposes `evaluate()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedStrategy {
    pub id: Uuid,
    pub hypothesis: Hypothesis,
    pub name: String,
}

// ---------------------------------------------------------------------------
// Backtest types
// ---------------------------------------------------------------------------

/// Snapshot of all signal features for one market at one synthetic tick.
/// Passed to `GeneratedStrategy::evaluate()` by the backtest runner.
#[derive(Debug, Clone)]
pub struct MarketSnapshot {
    pub market_id: String,
    /// Raw market probability (price).
    pub market_prob: f64,
    /// `posterior_prob − market_prob`.
    pub posterior_edge: f64,
    /// `graph_implied_prob − market_prob`.
    pub graph_arb_edge: f64,
    /// Rolling z-score of price deltas.
    pub temporal_z_score: f64,
    /// Shock magnitude in [0, 1]; 0.0 if no shock.
    pub shock_magnitude: f64,
    /// Rolling standard deviation of price.
    pub volatility: f64,
    /// Raw posterior probability (used for direction and Kelly sizing).
    pub posterior_prob: f64,
    /// Raw graph-implied probability (from BFS-neighbour correlation).
    pub implied_graph_prob: f64,
}

/// One completed simulated trade from a backtest.
#[derive(Debug, Clone)]
pub struct BacktestTrade {
    pub market_id: String,
    pub direction: TradeDirection,
    /// Fraction of `initial_capital` allocated to this trade.
    pub position_fraction: f64,
    pub entry_prob: f64,
    pub exit_prob: f64,
    /// PnL as a fraction of position notional: `(exit − entry) × sign`.
    pub pnl_fraction: f64,
    /// Absolute PnL in capital units: `pnl_fraction × position_fraction × initial_capital`.
    pub pnl_abs: f64,
}

/// Complete result of running a strategy over a synthetic backtest path.
#[derive(Debug, Clone)]
pub struct BacktestResult {
    pub strategy_id: Uuid,
    pub trades: Vec<BacktestTrade>,
    /// Equity snapshot at each tick: `initial_capital + Σ realised_pnl_so_far`.
    pub equity_curve: Vec<f64>,
    pub initial_capital: f64,
}

// ---------------------------------------------------------------------------
// Evaluation and registry types
// ---------------------------------------------------------------------------

/// Performance metrics computed from a `BacktestResult`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyMetrics {
    pub strategy_id: Uuid,
    /// Annualised Sharpe ratio (√252 convention).
    pub sharpe_ratio: f64,
    /// Peak-to-trough drawdown as a fraction of peak equity.
    pub max_drawdown: f64,
    /// Fraction of trades with `pnl_abs > 0`.
    pub win_rate: f64,
    pub trade_count: usize,
    /// `(final_equity − initial_capital) / initial_capital`.
    pub total_return: f64,
    /// Standard deviation of per-tick equity returns.
    pub volatility: f64,
    /// Mean absolute PnL per trade.
    pub avg_trade_pnl: f64,
}

/// A strategy that passed evaluation thresholds and lives in the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredStrategy {
    /// UUID string matching `hypothesis.id`.
    pub id: String,
    pub hypothesis: Hypothesis,
    pub metrics: StrategyMetrics,
    pub promoted_at: DateTime<Utc>,
}
