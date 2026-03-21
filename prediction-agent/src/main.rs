// prediction-agent/src/main.rs
// Workspace entry point.
//
// Responsibilities:
//   1. Load TOML configuration
//   2. Initialise structured logging (tracing) and Prometheus metrics
//   3. Create the shared Event Bus
//   4. Instantiate all engines and agents
//   5. Spawn each as an independent async task
//   6. Wait for shutdown signal (Ctrl-C)

use anyhow::Result;
use common::EventBus;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::info;

// ── Engines ─────────────────────────────────────────────────────────────────
use bayesian_engine::BayesianModel;
use execution_sim::ExecutionSim;
use market_graph::MarketGraph;
use execution_sim::ExecutionConfig;
use risk_engine::{RiskConfig, RiskEngine};
use simulation_engine::{SimulationConfig, SimulationEngine};

// ── Agents ───────────────────────────────────────────────────────────────────
use bayesian_edge_agent::{BayesianEdgeAgent, BayesianEdgeConfig};
use graph_arb_agent::{GraphArbAgent, GraphArbConfig};
use market_scanner::{KalshiClient, MarketScanner, PolymarketClient, ScannerConfig};
use signal_agent::{SignalAgent, SignalAgentConfig};
use temporal_agent::{TemporalAgent, TemporalConfig};

// ── Shock detector ────────────────────────────────────────────────────────────
use shock_detector::{InformationShockDetector, ShockDetectorConfig};

// ── Meta strategy ─────────────────────────────────────────────────────────────
use meta_strategy::{MetaStrategyConfig, MetaStrategyEngine};

// ── Relationship discovery ────────────────────────────────────────────────────
use relationship_discovery::{RelationshipDiscoveryConfig, RelationshipDiscoveryEngine};

// ── Strategy research ─────────────────────────────────────────────────────────
use strategy_research::{ResearchConfig, StrategyResearchEngine};

// ── Data ingestion layer ───────────────────────────────────────────────────
use data_ingestion::{DataIngestionEngine, IngestionConfig};

// ── Control panel ─────────────────────────────────────────────────────────────
use control_panel::{AppState as ControlPanelState, ControlPanelServer};

// ── Pipeline engines (previously unwired) ────────────────────────────────────
use performance_analytics::{AnalyticsConfig, PerformanceAnalytics};
use portfolio_engine::{PortfolioConfig, PortfolioEngine};
use portfolio_optimizer::{AllocationConfig, PortfolioOptimizer};
use signal_priority_engine::{PriorityConfig, SignalPriorityEngine};
use calibration::{CalibrationConfig, CalibrationEngine};

// ── Config ───────────────────────────────────────────────────────────────────
mod app_config;
use app_config::AppConfig;

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env before anything else so API keys are visible to all connectors.
    let _ = dotenvy::dotenv();

    // ── 1. Logging ─────────────────────────────────────────────────────────
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "prediction_agent=info,common=info".parse().unwrap()),
        )
        .init();

    info!("prediction-agent: starting up");

    // ── 2. Metrics (Prometheus) ─────────────────────────────────────────────
    // Starts an HTTP server on :9000/metrics for Prometheus scraping.
    // TODO: make bind address configurable via AppConfig.
    let builder = metrics_exporter_prometheus::PrometheusBuilder::new();
    builder
        .install()
        .expect("failed to install Prometheus metrics exporter");
    info!("metrics: Prometheus exporter installed (scrape :9000/metrics)");

    // ── 3. Configuration ────────────────────────────────────────────────────
    let config = AppConfig::load().unwrap_or_else(|e| {
        tracing::warn!("config load failed ({e}), using defaults");
        AppConfig::default()
    });
    info!(?config, "prediction-agent: loaded configuration");

    // ── 4. Event Bus ────────────────────────────────────────────────────────
    // Single broadcast channel shared by all engines and agents.
    // Each component calls bus.clone() to get its own handle.
    let bus = EventBus::with_capacity(config.bus_capacity);
    info!("event_bus: initialised");

    // ── 5. Engines ──────────────────────────────────────────────────────────

    // market_graph — builds the dependency graph from MarketUpdate events,
    // emits GraphUpdate events consumed by bayesian_engine and arb_agent.
    let graph_engine = MarketGraph::new(bus.clone());

    // bayesian_engine — maintains model probabilities, subscribes to
    // MarketUpdate + GraphUpdate + SentimentUpdate.
    let bayes_engine = BayesianModel::new(bus.clone());

    // risk_engine — gates TradeSignals against exposure limits before they
    // reach execution_sim.
    let risk_cfg = RiskConfig {
        min_expected_value: config.arb_threshold,
        bankroll:           config.paper_bankroll,
        ..RiskConfig::default()
    };
    let risk_eng = RiskEngine::new(risk_cfg, bus.clone());

    // simulation_engine — Monte Carlo / backtesting; in default LiveObserver
    // mode it passively tracks Execution/Portfolio events from the bus.
    let sim_engine = SimulationEngine::new(SimulationConfig::default(), bus.clone());

    // execution_sim — simulated order fill; publishes ExecutionResult and
    // PortfolioUpdate. NO real orders are sent. paper_bankroll sets reference capital.
    let exec_sim = ExecutionSim::new(
        ExecutionConfig {
            bankroll: config.paper_bankroll,
            ..ExecutionConfig::default()
        },
        bus.clone(),
    );

    // ── 6. Agents ───────────────────────────────────────────────────────────

    // market_scanner — polls Polymarket + Kalshi APIs every tick, publishes
    // MarketUpdate events. This is the sole source of market data.
    let scanner_cfg = ScannerConfig {
        poll_interval_ms: config.tick_interval_ms,
        ..ScannerConfig::default()
    };
    let scanner = MarketScanner::new(bus.clone(), scanner_cfg.clone())
        .with_source(Box::new(
            PolymarketClient::new(
                &scanner_cfg.polymarket_base_url,
                scanner_cfg.request_timeout_ms,
            )
            .expect("failed to build Polymarket HTTP client"),
        ))
        .with_source(Box::new(
            KalshiClient::new(
                &scanner_cfg.kalshi_base_url,
                scanner_cfg.request_timeout_ms,
            )
            .expect("failed to build Kalshi HTTP client"),
        ));

    // signal_agent — converts Bayesian posteriors into sized TradeSignal events.
    let signal = SignalAgent::new(bus.clone(), SignalAgentConfig::default());

    // shock_detector — detects price and sentiment shocks, publishes Event::Shock.
    let shock_det = InformationShockDetector::new(ShockDetectorConfig::default(), bus.clone());

    // graph_arb_agent — detects arbitrage between graph-implied and market prices.
    let graph_arb = GraphArbAgent::new(GraphArbConfig::default(), bus.clone());

    // temporal_agent — detects momentum trends from rolling price history.
    let temporal = TemporalAgent::new(TemporalConfig::default(), bus.clone());

    // bayesian_edge_agent — Bayesian deviation signals with shock boost and graph damping.
    let bayes_edge = BayesianEdgeAgent::new(BayesianEdgeConfig::default(), bus.clone());

    // meta_strategy — fuses signals from all strategy agents into MetaSignals.
    let meta_strat = MetaStrategyEngine::new(MetaStrategyConfig::default(), bus.clone());

    // relationship_discovery — discovers statistical/semantic relationships between
    // markets from price co-movement, emits Event::RelationshipDiscovered.
    let rel_disc = RelationshipDiscoveryEngine::new(RelationshipDiscoveryConfig::default(), bus.clone());

    // strategy_research — automated hypothesis generation and backtesting loop.
    // Runs on a 5-minute timer; emits Event::StrategyDiscovered for promoted strategies.
    let strategy_research = StrategyResearchEngine::new(ResearchConfig::default(), bus.clone());

    // data_ingestion — collects market data, news, social trends, economic
    // releases, and calendar events from external APIs (stubs in sim mode).
    // Publishes Event::Market, MarketSnapshot, NewsEvent, SocialTrend,
    // EconomicRelease, and CalendarEvent to the bus.
    let data_ingestion = DataIngestionEngine::new(IngestionConfig::default(), bus.clone());

    // signal_priority_engine — classifies signals as fast (immediate) or slow
    // (batched), routing fast signals directly to risk_engine via FastSignal and
    // slow signals to portfolio_optimizer via TopSignalsBatch.
    let priority_engine = SignalPriorityEngine::new(PriorityConfig::default(), bus.clone());

    // portfolio_optimizer — batches TradeSignals and applies correlation penalty.
    // `use_priority_engine: true` makes it consume TopSignalsBatch instead of
    // raw Event::Signal, working in tandem with SignalPriorityEngine above.
    let port_opt = PortfolioOptimizer::new(
        AllocationConfig {
            bankroll: config.paper_bankroll,
            use_priority_engine: true,
            ..AllocationConfig::default()
        },
        bus.clone(),
    );

    // portfolio_engine — authoritative position and PnL tracker.
    let port_eng = PortfolioEngine::new(
        PortfolioConfig {
            initial_bankroll: config.paper_bankroll,
            ..PortfolioConfig::default()
        },
        bus.clone(),
    );

    // performance_analytics — Sharpe, drawdown, MTM PnL metrics.
    let analytics = PerformanceAnalytics::new(
        AnalyticsConfig {
            initial_bankroll: config.paper_bankroll,
            ..AnalyticsConfig::default()
        },
        bus.clone(),
    );

    // calibration — tracks posteriors vs resolved outcomes, emits CalibrationUpdate.
    let calibration_eng = CalibrationEngine::new(CalibrationConfig::default(), bus.clone());

    // ── 7. Cancellation token for graceful shutdown ─────────────────────────
    let cancel = CancellationToken::new();

    // ── 8. Control panel (Mission Control dashboard) ─────────────────────────
    // Taps the event bus to populate in-memory ring buffers and streams live
    // events to the Next.js frontend over WebSocket.
    let (cp_app_state, _) = ControlPanelState::new(config.paper_bankroll);
    let cp_state = Arc::new(cp_app_state);
    control_panel::event_collector::EventCollector::spawn(
        bus.clone(),
        cp_state.clone(),
        cancel.clone(),
    );
    let cp_server = ControlPanelServer::new(
        cp_state,
        "0.0.0.0:3001".parse().expect("invalid control-panel bind address"),
    );
    let h_control_panel = tokio::spawn(cp_server.run(cancel.clone()));
    info!("control_panel: server listening on http://0.0.0.0:3001");

    // ── 9. Spawn all tasks ──────────────────────────────────────────────────
    info!("prediction-agent: spawning engine and agent tasks");

    // Engines — scanner and graph have CancellationToken-aware run().
    // Other engines are stubs awaiting implementation.
    let h_graph = tokio::spawn(graph_engine.run(cancel.child_token()));
    let h_bayes = tokio::spawn(bayes_engine.run(cancel.child_token()));
    let h_risk  = tokio::spawn(risk_eng.run(cancel.child_token()));
    let h_sim   = tokio::spawn(sim_engine.run(cancel.child_token()));
    let h_exec  = tokio::spawn(exec_sim.run(cancel.child_token()));

    // Agents
    let h_scanner  = tokio::spawn(scanner.run(cancel.child_token()));
    let h_signal      = tokio::spawn(signal.run(cancel.child_token()));
    let h_shock_det   = tokio::spawn(shock_det.run(cancel.child_token()));
    let h_graph_arb   = tokio::spawn(graph_arb.run(cancel.child_token()));
    let h_temporal    = tokio::spawn(temporal.run(cancel.child_token()));
    let h_bayes_edge  = tokio::spawn(bayes_edge.run(cancel.child_token()));
    let h_meta_strat  = tokio::spawn(meta_strat.run(cancel.child_token()));
    let h_rel_disc       = tokio::spawn(rel_disc.run(cancel.child_token()));
    let h_strategy_res   = tokio::spawn(strategy_research.run(cancel.child_token()));
    let h_data_ingestion = tokio::spawn(data_ingestion.run(cancel.child_token()));
    let h_priority_eng   = tokio::spawn(priority_engine.run(cancel.child_token()));
    let h_port_opt       = tokio::spawn(port_opt.run(cancel.child_token()));
    let h_port_eng       = tokio::spawn(port_eng.run(cancel.child_token()));
    let h_analytics      = tokio::spawn(analytics.run(cancel.child_token()));
    let h_calibration    = tokio::spawn(calibration_eng.run(cancel.child_token()));

    // ── 9. Await shutdown ───────────────────────────────────────────────────
    info!("prediction-agent: all tasks running — press Ctrl-C to stop");
    tokio::signal::ctrl_c().await?;
    info!("prediction-agent: shutdown signal received, cancelling tasks");

    // Signal the scanner and graph to exit cleanly.
    cancel.cancel();

    // Drop remaining stub tasks (they will be cancelled on process exit).
    drop((
        h_graph, h_bayes, h_risk, h_sim, h_exec,
        h_scanner,
        h_signal, h_shock_det, h_graph_arb, h_temporal, h_bayes_edge, h_meta_strat,
        h_rel_disc, h_strategy_res, h_data_ingestion,
        h_priority_eng, h_port_opt, h_port_eng, h_analytics,
        h_calibration, h_control_panel,
    ));

    Ok(())
}
