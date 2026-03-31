# Arbitrage Event Engine

A modular, event-driven prediction market trading system written in Rust. The bot exploits pricing inefficiencies on [Polymarket](https://polymarket.com) and [Kalshi](https://kalshi.com) using Bayesian probability updates, market graph analysis, multi-agent signal fusion, and a **net-edge cost model**. Supports both **simulated paper trading** (`execution_sim`) and a **live execution layer** (`oms` with exchange connectors).

## Overview

The system is a **Rust workspace** of **30 crates** (engines, agents, dashboard, test harnesses). Components communicate only through a shared **`EventBus`** (tokio broadcast channel). A **Mission Control** web stack (Axum + Next.js) provides live visibility into signals, portfolio state, market data, and strategy research.

On startup, `prediction-agent` loads **`.env`** (via `dotenvy`) so API keys are available to connectors before any tasks spawn.

### Strategy approach

- Markets are modeled as a **probability graph** — nodes are markets, edges are correlations and dependencies.
- **Bayesian inference** fuses market prices, graph propagation, sentiment (when present), and **economic releases** (`Event::EconomicRelease`) into posterior estimates.
- **Strategy agents** emit `Event::Signal`:
  - `signal_agent` — Kelly-sized positions from posteriors
  - `graph_arb_agent` — graph-implied vs. market-price arbitrage (including cross-platform logic where configured)
  - `temporal_agent` — rolling z-score momentum
  - `bayesian_edge_agent` — posterior deviation with shock boost and graph damping
- **`signal_priority_engine`** splits signals into a **fast path** (`Event::FastSignal` → `risk_engine`) and a **slow path** (`Event::TopSignalsBatch` → `portfolio_optimizer` → `Event::OptimizedSignal` → `risk_engine`).
- **`meta_strategy`** fuses raw agent signals into **`Event::MetaSignal`** for the **control panel** (observability); the default execution path uses the priority engine + optimizer + risk stack above.
- **`strategy_research`** runs hypothesis generation and backtesting on a timer (default ~5 minutes), promoting strategies and emitting `Event::StrategyDiscovered`.
- **`risk_engine`** applies exposure limits, drawdown gates, and the **`cost_model`** (fees, spread, slippage, decay) so only **positive net edge** trades pass when configured.
- **`execution_sim`** simulates fills with slippage; **`portfolio_engine`** is the source of truth for positions and PnL.

Target niches (config): crypto, politics, geopolitics, macro, sports.

---

## Architecture

### Production event flow (`prediction-agent`)

The orchestrator **spawns** all components below, including **`world_model`** (probabilistic constraint propagation) and **`scenario_engine`** (Monte Carlo scenario generation).

```
market_scanner ──→ Event::Market (Polymarket + Kalshi, every tick)

data_ingestion ──→ Event::Market / MarketSnapshot / NewsEvent / SocialTrend
                   EconomicRelease / CalendarEvent
                   (NewsAPI, Alpha Vantage, FRED, Reddit, Twitter, Kalshi/Polymarket
                    ingestion connectors — requires API keys in .env where applicable)

                        ├─→ market_graph     ──→ Event::Graph
                        │       └─→ bayesian_engine ──→ Event::Posterior
                        ├─→ shock_detector   ──→ Event::Shock
                        └─→ world_model ──→ Event::WorldProbability / WorldSignal / Inconsistency
                                └─→ scenario_engine ──→ Event::ScenarioBatch / ScenarioSignal

bayesian_engine  ──→ Event::Posterior
                        ├─→ signal_agent         ──→ Event::Signal
                        └─→ bayesian_edge_agent  ──→ Event::Signal

graph_arb_agent / temporal_agent / bayesian_edge_agent / signal_agent
                        └─→ Event::Signal
                                └─→ signal_priority_engine
                                        ├─→ Event::FastSignal ──────────────→ risk_engine
                                        └─→ Event::TopSignalsBatch
                                                └─→ portfolio_optimizer ──→ Event::OptimizedSignal
                                                                                └─→ risk_engine

meta_strategy    ──→ Event::MetaSignal  (Mission Control / analytics)

risk_engine      ──→ Event::ApprovedTrade (+ Event::TradeRejected when gated)
                        ├─→ execution_sim  ──→ Event::Execution  (paper trading)
                        └─→ oms            ──→ Event::Execution  (live/dry-run via exchange connectors)
                                └─→ portfolio_engine ──→ Event::Portfolio
                                        ├─→ performance_analytics
                                        └─→ calibration ──→ Event::CalibrationUpdate

relationship_discovery  consumes Event::Market → Event::RelationshipDiscovered

simulation_engine       default LiveObserver: tracks Execution / Portfolio (no interference)
```

### Workspace layout

```
crates/
  common/                 # Event enum, EventBus, TradeSignal, shared wire types
  market_graph/           # petgraph + BFS probability propagation
  bayesian_engine/        # Log-odds posterior fusion (+ EconomicRelease path)
  world_model/            # Probabilistic constraint propagation + inconsistency detection
  scenario_engine/        # Monte Carlo scenario generation + expectation analysis
  shock_detector/         # Shocks → Event::Shock
  meta_strategy/          # Multi-agent fusion → Event::MetaSignal
  strategy_research/      # Hypothesis gen, backtests, strategy registry
  risk_engine/            # Exposure + drawdown + cost_model net-edge gates
  cost_model/             # Fee/spread/slippage/decay estimates (used by risk_engine)
  portfolio_optimizer/    # Batch allocation + correlation penalty (+ priority-engine mode)
  signal_priority_engine/ # Fast vs slow signal routing
  execution_sim/          # Simulated fills
  portfolio_engine/       # Positions + PnL
  performance_analytics/  # Sharpe, drawdown, MTM; Prometheus gauges
  calibration/            # Resolved-market calibration → Event::CalibrationUpdate
  simulation_engine/      # Live observer, historical replay, Monte Carlo helpers
  relationship_discovery/ # Correlation / relationship discovery
  exchange_api/           # ExchangeConnector trait + Polymarket/Kalshi connectors (retry, config)
  oms/                    # Order Management System — live/dry-run execution
  persistence/            # SQLite trade history, equity curves, audit trail
  data_ingestion/         # External APIs (with retry), normalizers, snapshot cache, matcher

agents/
  market_scanner/         # Polymarket + Kalshi REST polling
  signal_agent/
  graph_arb_agent/
  temporal_agent/
  bayesian_edge_agent/

control-panel/            # Axum REST + WebSocket (default :3001)
control-panel-ui/         # Next.js dashboard (dev :3000)
prediction-agent/         # Binary orchestrator

tests/
  integration_harness/    # Synthetic end-to-end pipeline
  cost_model_backtest/    # Before/after cost gating Monte Carlo comparison (`cargo run -p cost_model_backtest`)
```

---

## Mission Control dashboard

Live web UI with multiple pages (overview, markets, signals, portfolio, strategy research, risk controls, execution). Backend streams **`WsEvent`** JSON on **`/ws/stream`**.

| Page | Description |
|------|-------------|
| **System Overview** | Uptime, throughput, agent status, market counts |
| **Market Intelligence** | Live probabilities, liquidity, update rates |
| **Signal Monitor** | Signal feed: edge, confidence, direction |
| **Portfolio** | Equity, PnL, drawdown, Sharpe, positions |
| **Strategy Research** | Promoted strategies and backtest metrics |
| **Risk Control** | Runtime parameter updates (where implemented) |
| **Execution Monitor** | Fills, slippage, fill ratios |

**Frontend:** `http://localhost:3000` (after `npm run dev` in `control-panel-ui`).  
**Backend:** `http://0.0.0.0:3001` (started automatically with `prediction-agent`).

Set `NEXT_PUBLIC_WS_URL=ws://localhost:3001/ws/stream` in `control-panel-ui/.env.local` for WebSocket in dev (Next rewrites do not upgrade WS).

### Metrics (`:9000/metrics`)

Prometheus scrape endpoint (started with the binary). Includes pipeline counters (scanner, graph, Bayesian, priority engine, strategy research, etc.) and **portfolio gauges** after `Event::Portfolio` updates (e.g. `portfolio_total_pnl`, `portfolio_sharpe_ratio`, `portfolio_max_drawdown`, `portfolio_equity`). Counters/gauges are **in-memory** and reset on process restart.

### Metrics alignment (analytics)

`performance_analytics` default **`tick_interval_secs`** is **60** (one-minute snapshot cadence for Sharpe annualisation), so the annualisation factor is **`√(31,557,600 / 60) ≈ 725`**, matching the dashboard narrative when both use the same tick assumption.

---

## Getting started

### Prerequisites

- **Rust** (stable, 2021 edition) — [rustup](https://rustup.rs)
- **Node.js** 18+ and **npm** — for the dashboard frontend

### Build

```bash
cargo check --workspace
cargo build --workspace
cargo build --workspace --release
```

### Run

```bash
# Full stack: all engines/agents + control panel :3001 + Prometheus :9000
cargo run --bin prediction-agent

RUST_LOG=debug cargo run --bin prediction-agent

# Synthetic integration test (no external APIs)
cargo run --bin integration_harness

# Cost-model before/after backtest (standalone binary)
cargo run -p cost_model_backtest
```

### Dashboard

```bash
cd control-panel-ui
npm install
npm run dev    # http://localhost:3000
```

### Configuration

| Source | Purpose |
|--------|---------|
| `config/default.toml` | Committed defaults (`arb_threshold`, `tick_interval_ms`, `paper_bankroll`, `bus_capacity`, `[scanner]`, `[graph]`, …) |
| `config/local.toml` | Gitignored overrides |
| `PRED_AGENT_*` env vars | Override TOML (e.g. `PRED_AGENT_PAPER_BANKROLL`) |
| **`.env`** | **API keys** (gitignored). Loaded at startup by `prediction-agent`. |

| Key | Default | Description |
|-----|---------|-------------|
| `arb_threshold` | `0.05` | Minimum model vs. market gap for risk / signal context |
| `tick_interval_ms` | `1000` | Market scanner poll interval |
| `paper_bankroll` | `10000` | Simulated USD capital (portfolio, execution, risk, optimizer, analytics) |
| `bus_capacity` | `1024` | EventBus channel capacity |
| `prometheus_bind_addr` | `0.0.0.0:9000` | Prometheus metrics exporter bind address |
| `control_panel_bind_addr` | `0.0.0.0:3001` | Control panel HTTP server bind address |

**Optional `.env` variables** (used by connectors when set):

| Variable | Used for |
|----------|----------|
| `POLYMARKET_API_KEY` / `_SECRET` / `_PASSPHRASE` | Polymarket CLOB API (live trading) |
| `KALSHI_API_KEY` / `_API_SECRET` | Kalshi Trading API (live trading) |
| `NEWS_API_KEY` | NewsAPI.org |
| `ALPHA_VANTAGE_KEY` | Alpha Vantage news/sentiment |
| `FRED_API_KEY` | St. Louis Fed economic series |
| `TRADING_ECONOMICS_KEY` | Trading Economics (if enabled) |
| `TWITTER_BEARER_TOKEN` | X/Twitter API (if enabled) |
| `CONTROL_PANEL_API_KEY` | Bearer token for dashboard auth |
| `CONTROL_PANEL_AUTH_DISABLED` | Set `true` to explicitly disable auth (fail-closed otherwise) |
| `LIVE_TRADING` | Set `true` to enable live order execution via OMS |
| `PERSISTENCE_DB_PATH` | SQLite database path (default: `data/arbitrage.db`) |

If a data API key is missing, the corresponding connector **skips** that source (warns in logs) rather than failing. Exchange client failures are also non-fatal — the scanner continues with whatever sources succeed.

---

## Testing

```bash
cargo test --workspace
cargo test -p bayesian_engine
cargo test -p meta_strategy test_signal_fusion -- --nocapture
cargo run --bin integration_harness
```

Integration tests use a real `EventBus`; subscribers are registered **before** spawning producers to avoid races.

### Test coverage (~450+ tests)

| Area | Crate(s) | Tests |
|------|----------|-------|
| Core infrastructure | `common` (EventBus) | 6 |
| Bayesian pipeline | `bayesian_engine`, `market_graph` | 34, 25 |
| World / scenario | `world_model`, `scenario_engine` | 17, 25 |
| Strategy agents | `signal_agent`, `graph_arb_agent`, `temporal_agent`, `bayesian_edge_agent` | 33, 23, 17, 19 |
| Meta / research | `meta_strategy`, `strategy_research` | 18, 43 |
| Signal routing | `signal_priority_engine` | 7 |
| Calibration | `calibration` | 8 |
| Execution | `oms` (with mock exchange), `simulation_engine` | 6, 47 |
| Data path | `data_ingestion`, `cost_model`, `portfolio_engine`, `performance_analytics` | 19, 15, 10, 33 |
| Other | `shock_detector`, `relationship_discovery`, `persistence` | 11, 20, 9 |

---

## Technology stack

**Rust:** `tokio`, `axum 0.7`, `petgraph`, `serde`/`serde_json`, `reqwest` (rustls), `tokio-tungstenite` (rustls), `tracing`, `metrics` + `metrics-exporter-prometheus`, `chrono`, `ndarray`, `tokio-util`, `dotenvy`, `futures`, `async-trait`, `parking_lot`, `anyhow`, `thiserror`.

**Frontend:** Next.js 14, Recharts, SWR, Tailwind CSS, lucide-react.

---

## Architecture patterns

1. **`config.rs`** — typed config + `Default` + `validate()`
2. **`new(config, bus) -> Result<Self, anyhow::Error>`** — validate at construction, return `Result` (never panic)
3. **`run(self, cancel: CancellationToken)`** — async loop with `tokio::select!` where applicable
4. **Pure core** — mutate `&mut State` without holding locks across `.await`
5. **Tests** — real `EventBus`; subscribe before spawn
6. **Retry** — exchange and data connectors use exponential backoff with configurable `max_retries` and `retry_base_delay_ms`

**Lock discipline:** write lock → mutate → snapshot → drop lock before I/O. Uses `parking_lot::Mutex`/`RwLock` (no poisoning).

---

## Implementation status

| Component | Status |
|-----------|--------|
| `common`, `market_graph`, `bayesian_engine` | Active in `prediction-agent` |
| `world_model`, `scenario_engine` | **Active** — probabilistic constraints + Monte Carlo scenarios |
| `data_ingestion` | Active with retry logic; requires keys in `.env` for full external coverage |
| `signal_priority_engine`, `portfolio_optimizer`, `risk_engine`, `cost_model` | Active; risk uses **net-edge** gating via `cost_model` |
| `execution_sim` | Active (paper trading) |
| `oms` + `exchange_api` | Active (dry-run by default; set `LIVE_TRADING=true` for live execution). Retry with exponential backoff, configurable URLs/fees via `ConnectorConfig`. |
| `portfolio_engine`, `performance_analytics`, `calibration` | Active |
| `persistence` | Active — SQLite trade history, equity curves, audit trail (`parking_lot::Mutex`) |
| `shock_detector`, strategy agents, `meta_strategy`, `strategy_research`, `relationship_discovery` | Active |
| `simulation_engine` | Default **LiveObserver**; **Monte Carlo** / **HistoricalReplay** / helpers available via config |
| `control-panel` + `control-panel-ui` | Active — fail-closed auth, metrics behind auth, constant-time token comparison |
| `tests/cost_model_backtest` | Standalone Monte Carlo comparison for cost gating |

### Security

- **Auth**: Fail-closed — requires `CONTROL_PANEL_API_KEY` or explicit `CONTROL_PANEL_AUTH_DISABLED=true`. Returns 503 when unconfigured.
- **Metrics**: `/api/metrics` is behind auth middleware; `/api/system/health` remains public for probes.
- **Token comparison**: Constant-time to prevent timing attacks.
- **Error handling**: All engine constructors return `Result` (no panics on bad config). Startup failures degrade gracefully where possible.

---

## Disclaimer

This project supports both **simulation** and **live trading**. Paper trading via `execution_sim` is the default. Enabling live trading (`LIVE_TRADING=true`) executes real orders through exchange connectors. Use at your own risk and ensure compliance with exchange terms and regulations.
