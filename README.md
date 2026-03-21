# Arbitrage Event Engine

A modular, event-driven prediction market trading system written in Rust. The bot exploits pricing inefficiencies on [Polymarket](https://polymarket.com) and [Kalshi](https://kalshi.com) using Bayesian probability updates, market graph analysis, multi-agent signal fusion, and a **net-edge cost model** — **all in simulated paper trading with no real order execution**.

## Overview

The system is a **Rust workspace** of **28 packages** (engines, agents, dashboard, test harnesses). Components communicate only through a shared **`EventBus`** (tokio broadcast channel). A **Mission Control** web stack (Axum + Next.js) provides live visibility into signals, portfolio state, market data, and strategy research.

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

The orchestrator **spawns** the components below. Crates such as **`world_model`** and **`scenario_engine`** exist in the workspace (types, tests) but are **not** currently wired into `prediction-agent`; their `Event::*` variants are reserved for future integration.

```
market_scanner ──→ Event::Market (Polymarket + Kalshi, every tick)

data_ingestion ──→ Event::Market / MarketSnapshot / NewsEvent / SocialTrend
                   EconomicRelease / CalendarEvent
                   (NewsAPI, Alpha Vantage, FRED, Reddit, Twitter, Kalshi/Polymarket
                    ingestion connectors — requires API keys in .env where applicable)

                        ├─→ market_graph     ──→ Event::Graph
                        │       └─→ bayesian_engine ──→ Event::Posterior
                        ├─→ shock_detector   ──→ Event::Shock
                        └─→ (world_model / scenario_engine — not spawned in main binary)

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
                        └─→ execution_sim  ──→ Event::Execution
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
  world_model/            # Global state + constraints (library; not in main binary)
  scenario_engine/        # Monte Carlo scenarios (library; not in main binary)
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
  data_ingestion/         # External APIs, normalizers, snapshot cache, matcher
  ...                     # (see Cargo.toml for full member list)

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

**Optional `.env` variables** (used by `data_ingestion` connectors when set):

| Variable | Used for |
|----------|----------|
| `NEWS_API_KEY` | NewsAPI.org |
| `ALPHA_VANTAGE_KEY` | Alpha Vantage news/sentiment |
| `FRED_API_KEY` | St. Louis Fed economic series |
| `TRADING_ECONOMICS_KEY` | Trading Economics (if enabled) |
| `TWITTER_BEARER_TOKEN` | X/Twitter API (if enabled) |

If a key is missing, the corresponding connector typically **skips** that source (warns in logs) rather than failing the whole process.

---

## Testing

```bash
cargo test --workspace
cargo test -p bayesian_engine
cargo test -p meta_strategy test_signal_fusion -- --nocapture
cargo run --bin integration_harness
```

Integration tests use a real `EventBus`; subscribers are registered **before** spawning producers to avoid races.

### Test coverage (representative)

| Area | Crate(s) |
|------|----------|
| World / scenario | `world_model`, `scenario_engine` |
| Meta / research | `meta_strategy`, `strategy_research` |
| Agents | `graph_arb_agent`, `temporal_agent`, `bayesian_edge_agent`, `signal_agent` |
| Data path | `data_ingestion`, `risk_engine`, `portfolio_engine`, … |

---

## Technology stack

**Rust:** `tokio`, `axum 0.7`, `petgraph`, `serde`/`serde_json`, `reqwest`, `tokio-tungstenite`, `tracing`, `metrics` + `metrics-exporter-prometheus`, `chrono`, `ndarray`, `tokio-util`, `dotenvy`, `futures`, `async-trait`.

**Frontend:** Next.js 14, Recharts, SWR, Tailwind CSS, lucide-react.

---

## Architecture patterns

1. **`config.rs`** — typed config + `Default` + `validate()`
2. **`new(config, bus)`** — validate at construction
3. **`run(self, cancel: CancellationToken)`** — async loop with `tokio::select!` where applicable
4. **Pure core** — mutate `&mut State` without holding locks across `.await`
5. **Tests** — real `EventBus`; subscribe before spawn

**Lock discipline:** write lock → mutate → snapshot → drop lock before I/O.

---

## Implementation status

| Component | Status |
|-----------|--------|
| `common`, `market_graph`, `bayesian_engine` | Active in `prediction-agent` |
| `data_ingestion` | Active; requires keys in `.env` for full external coverage |
| `signal_priority_engine`, `portfolio_optimizer`, `risk_engine`, `cost_model` | Active; risk uses **net-edge** gating via `cost_model` |
| `execution_sim`, `portfolio_engine`, `performance_analytics` | Active |
| `calibration` | Active |
| `shock_detector`, strategy agents, `meta_strategy`, `strategy_research`, `relationship_discovery` | Active |
| `simulation_engine` | Default **LiveObserver**; **Monte Carlo** / **HistoricalReplay** / helpers available via config — not a single “zeroed stub” anymore |
| `world_model`, `scenario_engine` | **Crates complete; not spawned** by `prediction-agent` today |
| `control-panel` + `control-panel-ui` | Active |
| `tests/cost_model_backtest` | Standalone Monte Carlo comparison for cost gating |

---

## Disclaimer

This project is for **research and simulation**. It does **not** execute real trades. All orders are simulated in `execution_sim`. Real trading would require a compliant execution layer and adherence to exchange terms and regulations.
