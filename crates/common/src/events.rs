// crates/common/src/events.rs
// All event variants that flow through the Event Bus.

use std::collections::HashMap;

use crate::types::{MarketNode, Portfolio, PrioritizedSignal, Timestamp, TradeDirection, TradeSignal};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Individual event payloads
// ---------------------------------------------------------------------------

/// Emitted by market_scanner whenever a market's state changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketUpdate {
    pub market: MarketNode,
}

/// A single node's implied-probability change within one propagation pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeProbUpdate {
    /// Market whose implied probability changed.
    pub market_id: String,
    /// `implied_prob` before this propagation pass.
    pub old_implied_prob: f64,
    /// `implied_prob` after this propagation pass.
    pub new_implied_prob: f64,
}

/// Emitted by market_graph after a BFS propagation pass completes.
///
/// One event is published per propagation trigger (i.e. per incoming
/// `MarketUpdate` that crosses the change threshold).  It carries every
/// downstream node whose `implied_prob` moved, together with the
/// before/after values so consumers can decide whether to act.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphUpdate {
    /// The market whose price change triggered this propagation pass.
    pub source_market_id: String,
    /// Every downstream node that was updated, with old and new implied probs.
    /// Empty when propagation was suppressed (delta below threshold).
    pub node_updates: Vec<NodeProbUpdate>,
}

/// Emitted by news_agent after ingesting and scoring a news item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentimentUpdate {
    /// Markets this item is relevant to
    pub related_market_ids: Vec<String>,
    /// Headline or summary
    pub headline: String,
    /// Sentiment score [-1.0 = very negative, 1.0 = very positive]
    pub sentiment_score: f64,
    pub timestamp: Timestamp,
}

/// Emitted by execution_sim after simulating an `ApprovedTrade`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    /// The risk-engine-approved trade that triggered this execution.
    pub trade: ApprovedTrade,
    /// `true` when `executed_quantity > 0`.
    pub filled: bool,
    /// Fraction of `trade.approved_fraction` that was actually executed [0, 1].
    pub fill_ratio: f64,
    /// Executed size as a fraction of bankroll (= `approved_fraction × fill_ratio`).
    pub executed_quantity: f64,
    /// Effective fill price in probability units, accounting for slippage.
    pub avg_price: f64,
    /// Signed price deviation: `(avg_price − market_price) / market_price`.
    pub slippage: f64,
    pub timestamp: Timestamp,
}

/// Emitted after portfolio state is recalculated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioUpdate {
    pub portfolio: Portfolio,
    pub timestamp: Timestamp,
}

/// Emitted by bayesian_engine after computing a new posterior for a market.
///
/// Consumed by signal_agent to convert Bayesian beliefs into sized trade
/// signals using Expected Value and Kelly position sizing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PosteriorUpdate {
    /// The market whose posterior was recomputed.
    pub market_id: String,
    /// Model prior probability before fusing any evidence.
    pub prior_prob: f64,
    /// New posterior probability after fusing all accumulated evidence.
    pub posterior_prob: f64,
    /// Last observed market price, if the engine has seen at least one
    /// `MarketUpdate` or `fuse_market_price` call for this market.
    pub market_prob: Option<f64>,
    /// `posterior_prob − market_prob` (or `− prior_prob` when no market price
    /// is available).
    ///
    /// * **Positive** → model is above market price → potential BUY signal.
    /// * **Negative** → model is below market price → potential SELL signal.
    pub deviation: f64,
    /// Evidence-density confidence in [0, 1]: `sqrt(history_len / MAX_HISTORY)`.
    ///
    /// Signals with `confidence < min_confidence` are suppressed by signal_agent.
    pub confidence: f64,
    pub timestamp: Timestamp,
}

/// Emitted by risk_engine after approving and (optionally) resizing a TradeSignal.
///
/// Consumed by execution_sim to simulate order placement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovedTrade {
    pub market_id: String,
    pub direction: TradeDirection,
    /// Final approved position fraction after all risk checks and resizing.
    pub approved_fraction: f64,
    pub expected_value: f64,
    pub posterior_prob: f64,
    pub market_prob: f64,
    /// When the originating `TradeSignal` was emitted by the strategy agent.
    /// Used by `execution_sim` to compute end-to-end execution latency.
    pub signal_timestamp: Timestamp,
    /// When the risk engine approved this trade.
    pub timestamp: Timestamp,
}

// ---------------------------------------------------------------------------
// Signal priority / fast-path events
// ---------------------------------------------------------------------------

/// Emitted by `SignalPriorityEngine` for high-edge signals that bypass the
/// `PortfolioOptimizer` batch window.
///
/// `RiskEngine` consumes `FastSignal` directly and evaluates it immediately,
/// regardless of the `accept_raw_signals` setting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FastSignal {
    pub signal:         TradeSignal,
    /// Ranking score used to classify this signal as fast.
    pub priority_score: f64,
}

/// Emitted periodically by `SignalPriorityEngine` containing the top-ranked
/// slow signals from the current batch window, sorted by priority descending.
///
/// Consumed by `PortfolioOptimizer` (when `use_priority_engine = true`) as a
/// replacement for individual `Event::Signal` events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopSignalsBatch {
    pub signals:    Vec<PrioritizedSignal>,
    /// Duration of the batch window this flush covers, in milliseconds.
    pub window_ms:  u64,
    pub timestamp:  Timestamp,
}

/// Emitted by `RiskEngine` when a signal is rejected, for missed-edge tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeRejected {
    pub market_id:     String,
    /// Human-readable rejection reason.
    pub reason:        String,
    /// `expected_value` of the rejected signal — counts toward missed_edge_total.
    pub expected_edge: f64,
    /// Originating strategy agent name.
    pub signal_source: String,
    pub timestamp:     Timestamp,
}

/// Emitted by `ExecutionSimulator` after each fill for latency and edge-decay
/// diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeDecayReport {
    pub market_id:            String,
    /// `expected_value` at signal-generation time.
    pub signal_expected_edge: f64,
    /// Milliseconds between signal emission and execution fill.
    /// Always non-negative; minor clock skew is clamped to zero.
    pub execution_latency_ms: u64,
    /// UTC timestamp of the original signal emission.
    pub signal_timestamp:     Timestamp,
    pub execution_timestamp:  Timestamp,
}

/// Emitted by portfolio_optimizer after batch-optimizing a tick's worth of TradeSignals.
///
/// Contains the original `TradeSignal` with `position_fraction` replaced by
/// the optimizer's `suggested_allocation`, plus metadata about the optimization.
/// Consumed by risk_engine for final exposure checks before approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizedSignal {
    /// Original signal with `position_fraction` set to the optimizer's suggested
    /// allocation (already capped by per-market, per-niche, and total-capital limits).
    pub signal: TradeSignal,
    /// `expected_value × confidence` — the optimizer's composite ranking metric.
    pub risk_adjusted_ev: f64,
    /// Names of strategy agents whose signals contributed to this allocation.
    pub source_strategies: Vec<String>,
    /// Fractional reduction applied due to intra-cluster correlation [0, 1].
    pub correlation_penalty: f64,
    /// Fraction of the niche budget consumed by other signals before this one.
    pub niche_utilization: f64,
}

/// Emitted by world_model after global belief propagation completes.
///
/// Carries the updated world-model probabilities for every market affected
/// by the propagation pass that was triggered by `source_market_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldProbabilityUpdate {
    /// The market whose update triggered this inference pass.
    pub source_market_id: String,
    /// Final world-model probabilities for every market touched by inference,
    /// including the source market itself.
    pub updated_probabilities: HashMap<String, f64>,
    /// Total number of belief-propagation edges traversed in this pass.
    pub propagation_count: usize,
    pub timestamp: Timestamp,
}

/// Emitted when a logical constraint between markets is violated.
///
/// Consumed by strategy agents and alerting systems. The violation is
/// characterised by how far the observed probabilities deviate from the
/// constraint's required relationship (e.g. mutual exclusion).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InconsistencyDetected {
    pub constraint_id: String,
    /// Human-readable constraint type, e.g. `"MutualExclusion"`.
    pub constraint_type: String,
    pub markets_involved: Vec<String>,
    /// Snapshot of each market's current world-model probability.
    pub current_probabilities: HashMap<String, f64>,
    /// Magnitude of the violation (e.g. |sum − 1| for mutual exclusion).
    pub violation_magnitude: f64,
    pub timestamp: Timestamp,
}

/// Higher-level signal emitted by world_model for strategy agents.
///
/// Combines the world-model's inferred probability with the raw market
/// price to surface an actionable edge. Only emitted when the edge
/// exceeds a minimum threshold.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldSignal {
    pub market_id: String,
    /// World-model inferred probability after global belief propagation.
    pub world_probability: f64,
    /// Last known raw market price, used as the benchmark.
    pub market_probability: f64,
    /// `world_probability − market_probability`.
    /// Positive → world model is bullish vs market → potential BUY.
    /// Negative → world model is bearish vs market → potential SELL.
    pub world_edge: f64,
    /// Confidence in the world-model estimate [0, 1].
    pub confidence: f64,
    pub timestamp: Timestamp,
}

// ---------------------------------------------------------------------------
// Information shock types (produced by shock_detector)
// ---------------------------------------------------------------------------

/// Direction of an information shock relative to market probability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShockDirection {
    /// Event is probability-increasing (bullish).
    Up,
    /// Event is probability-decreasing (bearish).
    Down,
    /// Direction is ambiguous or immaterial.
    Neutral,
}

/// Origin of the signal that produced this shock.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ShockSource {
    /// News article or headline.
    News,
    /// Social media activity or sentiment.
    SocialMedia,
    /// Macroeconomic indicator or data release.
    Macro,
    /// Market microstructure (sudden price volatility spike).
    Volatility,
    /// Other or unclassified source.
    Other(String),
}

/// An unusual or sudden information event that may move market probabilities.
///
/// Produced by `InformationShockDetector` from `MarketUpdate` and
/// `SentimentUpdate` events.  Magnitude and direction are fused from all
/// available signals for the given market.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InformationShock {
    /// Market this shock relates to.
    pub market_id: String,
    /// Relative strength of the shock in [0, 1].
    pub magnitude: f64,
    /// Whether the shock pushes the market up, down, or is neutral.
    pub direction: ShockDirection,
    /// Signal source that triggered detection.
    pub source: ShockSource,
    /// Raw z-score of the price delta (0.0 for sentiment-only shocks).
    pub z_score: f64,
    pub timestamp: Timestamp,
}

/// Emitted by scenario_engine after a Monte Carlo batch has been generated.
///
/// Contains metadata about the batch; the full batch data stays in the engine.
/// Consumers that need per-scenario detail subscribe to `ScenarioExpectations`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioBatchGenerated {
    /// Opaque batch identifier (microsecond UTC timestamp as a string).
    pub batch_id: String,
    pub sample_size: usize,
    pub market_count: usize,
    /// Wall-clock time taken to generate the batch, in milliseconds.
    pub generation_time_ms: u64,
    pub timestamp: Timestamp,
}

/// Computed marginal and joint expectations across a scenario batch.
///
/// `joint_probabilities` keys are `"<market_a>|<market_b>"` where
/// `market_a < market_b` lexicographically (symmetric pairs de-duplicated).
/// Only the top pairs by deviation from independence are included.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioExpectations {
    pub batch_id: String,
    /// Scenario-derived marginal probability P(market) for every market.
    pub expected_probabilities: HashMap<String, f64>,
    /// Top joint probabilities P(A ∧ B), encoded as `"A|B"` keys.
    pub joint_probabilities: HashMap<String, f64>,
    pub timestamp: Timestamp,
}

/// Emitted when a market's scenario-expected probability deviates meaningfully
/// from its raw market price — an actionable mispricing signal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioSignal {
    pub market_id: String,
    /// Scenario-derived expected probability P(event).
    pub expected_probability: f64,
    /// Current raw market price.
    pub market_probability: f64,
    /// `expected_probability − market_probability`.
    /// Positive → model above market → potential BUY.
    /// Negative → model below market → potential SELL.
    pub mispricing: f64,
    /// Confidence in the signal [0, 1].
    pub confidence: f64,
    pub timestamp: Timestamp,
}

/// Directionality of a discovered statistical relationship between two markets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeDirection {
    /// Neither market has a clearly stronger directional influence.
    Undirected,
    /// market_a conditionally leads / causes market_b.
    AtoB,
    /// market_b conditionally leads / causes market_a.
    BtoA,
}

/// A discovered relationship between two prediction markets.
///
/// Emitted by `RelationshipDiscoveryEngine` when pairwise statistical analysis
/// (correlation, mutual information, semantic similarity) finds co-movement
/// above the configured threshold.
///
/// Consumed by `market_graph` to dynamically add or reinforce correlation edges.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelationshipDiscovered {
    /// First market (canonical: market_a ≤ market_b lexicographically).
    pub market_a: String,
    /// Second market.
    pub market_b: String,
    /// Composite strength ∈ [0, 1]: weighted combination of correlation, MI, and embedding.
    pub strength: f64,
    /// Directionality inferred from conditional probability analysis.
    pub direction: EdgeDirection,
    /// Absolute Pearson r ∈ [0, 1].
    pub correlation: f64,
    /// Normalised mutual information ∈ [0, 1].
    pub mutual_information: f64,
    /// Cosine similarity between market-ID token embeddings ∈ [0, 1].
    pub embedding_similarity: f64,
    /// Number of paired price observations used for the estimates.
    pub sample_count: usize,
    pub timestamp: Timestamp,
}

/// Emitted by `meta_strategy` after fusing signals from multiple strategy agents.
///
/// `confidence` reflects the margin of directional agreement, weighted by
/// confidence × EV (1.0 = unanimous, 0.0 = perfectly split). May be boosted
/// by a recent `InformationShock`.
///
/// `expected_edge` is the confidence-weighted mean EV across all contributing signals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetaSignal {
    /// Market this meta signal targets.
    pub market_id: String,
    /// Resolved trade direction after weighing all strategy inputs.
    pub direction: TradeDirection,
    /// Combined confidence [0, 1]: directional-vote margin, optionally shock-boosted.
    pub confidence: f64,
    /// Confidence-weighted mean expected value across contributing signals.
    pub expected_edge: f64,
    /// Names of strategy agents that contributed (sorted, deduplicated).
    pub contributing_strategies: Vec<String>,
    /// Total number of strategy signals fused.
    pub signal_count: usize,
    pub timestamp: Timestamp,
}

/// Emitted by strategy_research when a generated strategy meets promotion criteria.
///
/// Consumed by meta_strategy (or any downstream consumer) to allocate capital to
/// the discovered strategy.  `rule_definition_json` is the JSON-serialized
/// `Hypothesis` so consumers can reconstruct and replay the strategy logic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyDiscoveredEvent {
    /// Unique strategy identifier (UUID as string).
    pub strategy_id: String,
    /// JSON-serialized `Hypothesis` — full rule definition.
    pub rule_definition_json: String,
    /// Annualised Sharpe ratio achieved in backtest.
    pub sharpe: f64,
    /// Maximum drawdown fraction observed during backtest.
    pub max_drawdown: f64,
    /// Fraction of trades that were profitable.
    pub win_rate: f64,
    /// Total number of simulated trades executed.
    pub trade_count: usize,
    /// UTC timestamp when the strategy was promoted to the registry.
    pub promoted_at: Timestamp,
}

// ---------------------------------------------------------------------------
// Data ingestion layer event payloads
// ---------------------------------------------------------------------------

/// Rich market snapshot emitted by the data_ingestion layer.
///
/// Carries the full market state including title, description, bid/ask spread,
/// and volume — a superset of the lightweight `MarketUpdate`/`MarketNode` used
/// by the graph and Bayesian engines.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketSnapshot {
    pub market_id: String,
    pub title: String,
    pub description: String,
    /// Current mid-market probability [0, 1].
    pub probability: f64,
    /// Best bid in probability units.
    pub bid: f64,
    /// Best ask in probability units.
    pub ask: f64,
    /// Total traded volume (notional units).
    pub volume: f64,
    /// Available liquidity (notional units).
    pub liquidity: f64,
    pub timestamp: Timestamp,
}

/// A news article or headline ingested from an external news API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsEvent {
    /// Deduplicated article identifier (URL hash or API-assigned ID).
    pub id: String,
    pub headline: String,
    pub body: String,
    pub source: String,
    pub timestamp: Timestamp,
    /// Named entities extracted from the article (people, orgs, locations).
    pub entities: Vec<String>,
    /// Topic tags (e.g. "politics", "economics", "sport").
    pub topics: Vec<String>,
}

/// An aggregated social media trend for a topic.
///
/// Emitted periodically by social connectors; represents the aggregate
/// signal over a rolling window rather than individual posts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocialTrend {
    /// Topic keyword or hashtag.
    pub topic: String,
    /// Mentions per minute over the rolling window.
    pub mention_rate: f64,
    /// Aggregate sentiment score [-1 = very negative, 1 = very positive].
    pub sentiment: f64,
    /// Rate of change in mentions (positive = accelerating).
    pub velocity: f64,
    pub timestamp: Timestamp,
}

/// A structured economic indicator release.
///
/// Emitted when a data-release connector fetches a new print (e.g. CPI, NFP).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EconomicRelease {
    /// Standard indicator name (e.g. "US_CPI_YOY", "US_NFP").
    pub indicator: String,
    /// Actual reported value.
    pub value: f64,
    /// Consensus forecast prior to release, if available.
    pub forecast: Option<f64>,
    /// Previous period's value, if available.
    pub previous: Option<f64>,
    pub timestamp: Timestamp,
}

/// A scheduled future event from the event calendar.
///
/// Examples: elections, court decisions, earnings calls, central bank meetings.
/// Consumers can pre-position ahead of `scheduled_time`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarEvent {
    pub event_id: String,
    pub title: String,
    /// When the event is expected to occur (UTC).
    pub scheduled_time: Timestamp,
    /// Category string, e.g. "election", "earnings", "central_bank", "court".
    pub category: String,
    /// Expected market-moving impact in [0, 1] (higher = more impactful).
    pub expected_impact: f64,
}

// ---------------------------------------------------------------------------
// Unified Event enum — the only type sent over the Event Bus
// ---------------------------------------------------------------------------

/// Every message on the bus is one of these variants.
/// All variants must be Clone so broadcast::Sender<Event> can fan-out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    // ---- data ingestion (market_scanner / data_ingestion layer) ----
    Market(MarketUpdate),
    MarketSnapshot(MarketSnapshot),
    NewsEvent(NewsEvent),
    SocialTrend(SocialTrend),
    EconomicRelease(EconomicRelease),
    CalendarEvent(CalendarEvent),

    // ---- engine outputs ----
    Graph(GraphUpdate),
    Sentiment(SentimentUpdate),

    // ---- strategy outputs ----
    Signal(TradeSignal),

    // ---- engine outputs (bayesian) ----
    Posterior(PosteriorUpdate),

    // ---- execution / portfolio ----
    Execution(ExecutionResult),
    Portfolio(PortfolioUpdate),

    // ---- portfolio optimizer output ----
    OptimizedSignal(OptimizedSignal),

    // ---- risk engine output ----
    ApprovedTrade(ApprovedTrade),

    // ---- world model outputs ----
    WorldProbability(WorldProbabilityUpdate),
    Inconsistency(InconsistencyDetected),
    WorldSignal(WorldSignal),

    // ---- scenario engine outputs ----
    ScenarioBatch(ScenarioBatchGenerated),
    ScenarioExpectations(ScenarioExpectations),
    ScenarioSignal(ScenarioSignal),

    // ---- shock detector output ----
    Shock(InformationShock),

    // ---- meta strategy output ----
    MetaSignal(MetaSignal),

    // ---- strategy research output ----
    StrategyDiscovered(StrategyDiscoveredEvent),

    // ---- relationship discovery output ----
    RelationshipDiscovered(RelationshipDiscovered),

    // ---- signal priority engine outputs ----
    /// High-edge signal bypassing portfolio_optimizer batch window.
    FastSignal(FastSignal),
    /// Batch of top-ranked slow signals from the priority engine.
    TopSignalsBatch(TopSignalsBatch),

    // ---- execution diagnostics ----
    /// Emitted when a signal is rejected; carries missed expected_edge.
    TradeRejected(TradeRejected),
    /// Per-fill latency and edge-decay report.
    EdgeDecayReport(EdgeDecayReport),
}
