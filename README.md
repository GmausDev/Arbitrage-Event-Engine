# Arbitrage Event Engine

A modular, event-driven prediction market trading system written in Rust. The bot exploits pricing inefficiencies on [Polymarket](https://polymarket.com) and [Kalshi](https://kalshi.com) using Bayesian probability updates, market graph analysis, and multi-agent signal fusion — **all in simulated paper trading with no real order execution**.

## Overview

The system is built as a Rust workspace of 25+ specialized crates, each responsible for a single concern, communicating exclusively through a shared `EventBus` (tokio broadcast channel). A real-time web dashboard provides live visibility into signals, portfolio state, market data, and strategy research.

### Strategy Approach

- Models markets as a **probability graph** — nodes are markets, edges are statistical correlations and logical dependencies
- **Bayesian inference** fuses market price data, sentiment signals, and graph-propagated information into posterior probability estimates
- Four **strategy agents** independently generate trade signals:
  - `signal_agent` — Kelly-sized positions from Bayesian posteriors
  - `graph_arb_agent` — graph-implied vs. market-price arbitrage
  - `temporal_agent` — rolling z-score momentum from price history
  - `bayesian_edge_agent` — posterior deviation with shock boost and graph damping
- `meta_strategy` fuses signals across all agents with weighted direction voting
- `strategy_research` runs automated hypothesis generation and backtesting on a 5-minute timer, promoting winning strategies
- `risk_engine` gates all signals through exposure limits and Kelly sizing before execution
- `execution_sim` simulates realistic order fills with slippage

Target market niches: crypto, politics, geopolitics, macro, sports.

---

## Architecture

### Event Flow

```
market_scanner ──→ Event::Market
                        ├─→ market_graph     ──→ Event::Graph
                        │       └─→ bayesian_engine ──→ Event::Posterior
                        ├─→ shock_detector   ──→ Event::Shock
                        └─→ world_model      ──→ Event::WorldProbability
                                └─→ scenario_engine ──→ Event::ScenarioSignal

news_agent       ──→ Event::Sentiment ──→ bayesian_engine
data_ingestion   ──→ Event::Market / MarketSnapshot / NewsEvent / SocialTrend

bayesian_engine  ──→ Event::Posterior
                        ├─→ signal_agent         ──→ Event::Signal
                        └─→ bayesian_edge_agent  ──→ Event::Signal

graph_arb_agent / temporal_agent / bayesian_edge_agent / signal_agent
                        └─→ Event::Signal
                                └─→ signal_priority_engine
                                └─→ meta_strategy  ──→ Event::MetaSignal
                                └─→ portfolio_optimizer ──→ Event::OptimizedSignal

risk_engine      ──→ Event::ApprovedTrade
                        └─→ execution_sim  ──→ Event::Execution
                                └─→ portfolio_engine ──→ Event::Portfolio
                                        └─→ performance_analytics
```

`relationship_discovery` passively consumes `Event::Market` and emits `Event::RelationshipDiscovered` (auto-discovered market correlations). `data_ingestion` is the authoritative source for production data.

### Workspace Layout

```
crates/
  common/                # Event enum, EventBus, TradeSignal, TradeDirection, shared types
  market_graph/          # petgraph correlation graph + BFS probability propagation
  bayesian_engine/       # Log-odds Bayesian posterior fusion
  world_model/           # Global probabilistic state + logical constraint enforcement
  scenario_engine/       # Two-pass Monte Carlo joint-market sampling
  shock_detector/        # Price/sentiment shock detection → Event::Shock
  meta_strategy/         # Multi-agent signal fusion → Event::MetaSignal
  strategy_research/     # Hypothesis gen, parallel backtesting, strategy registry
  risk_engine/           # Exposure gates, Kelly position sizing
  portfolio_optimizer/   # Batch signal allocation + correlation penalty
  execution_sim/         # Simulated fills with slippage model
  portfolio_engine/      # Authoritative position and PnL tracking
  performance_analytics/ # Sharpe, drawdown, MTM PnL metrics
  simulation_engine/     # Monte Carlo backtesting (placeholder)
  relationship_discovery/# Pearson correlation, mutual information, TF-IDF similarity
  data_ingestion/        # Polymarket/Kalshi/NewsAPI/Reddit/FRED connectors
  signal_priority_engine/# Fast (direct) vs. slow (batched) signal routing

agents/
  market_scanner/        # Polls Polymarket + Kalshi REST APIs every tick
  signal_agent/          # EV + Kelly sizing from posteriors → Event::Signal
  graph_arb_agent/       # Graph-implied vs. market-price arbitrage
  temporal_agent/        # Rolling momentum z-score signals
  bayesian_edge_agent/   # Posterior deviation + shock boost + graph damping
  news_agent/            # News sentiment stub
  arb_agent/             # Legacy arbitrage stub (superseded by strategy agents)

control-panel/           # Axum REST + WebSocket backend (port 3001)
control-panel-ui/        # Next.js/React dashboard frontend (port 3000)
prediction-agent/        # Binary orchestrator — wires everything, spawns all tasks
tests/
  integration_harness/   # Synthetic 3-market end-to-end pipeline test
```

---

## Mission Control Dashboard

A live web dashboard with 7 pages:

| Page | Description |
|------|-------------|
| **System Overview** | Uptime, event throughput, agent status, market count |
| **Market Intelligence** | Live market probabilities, bid/ask, liquidity, update rate |
| **Signal Monitor** | Real-time signal feed with edge, confidence, direction |
| **Portfolio** | Equity curve, PnL (realized/unrealized), drawdown, Sharpe, open positions |
| **Strategy Research** | Promoted strategy registry with backtest metrics |
| **Risk Control** | Live parameter sliders (edge threshold, Kelly fraction, position limits) with zero-restart updates |
| **Execution Monitor** | Fill history, slippage distribution, fill ratios |

The backend streams `WsEvent` JSON over WebSocket (`/ws/stream`). Metrics also expose over Prometheus on `:9000/metrics`.

### Metrics Alignment

Dashboard metrics are computed to match `performance_analytics` exactly:
- **Sharpe**: absolute PnL changes per tick, sample variance (n−1), annualisation factor `√(31,557,600 / 60) ≈ 725`
- **Drawdown**: lifetime global high-water mark (never decreases within a session)
- **PnL split**: MTM unrealized computed from position entry price vs. current market price

---

## Getting Started

### Prerequisites

- **Rust** (stable, 2021 edition) — [install via rustup](https://rustup.rs)
- **Node.js** 18+ and npm — for the dashboard frontend

### Build

```bash
# Check the full workspace
cargo check --workspace

# Build (debug)
cargo build --workspace

# Build (release)
cargo build --workspace --release
```

### Run

```bash
# Start the bot (includes control panel backend on :3001 and Prometheus on :9000)
cargo run --bin prediction-agent

# With debug logging
RUST_LOG=debug cargo run --bin prediction-agent

# Run the end-to-end integration test (no external APIs required)
cargo run --bin integration_harness
```

### Dashboard

```bash
cd control-panel-ui

# Install dependencies (first time only)
npm install

# Development server with hot reload (proxies /api/* to :3001)
npm run dev        # http://localhost:3000

# Production build
npm run build && npm start
```

**WebSocket connection**: The frontend connects directly to the Rust backend WebSocket. Set `NEXT_PUBLIC_WS_URL=ws://localhost:3001/ws/stream` in `control-panel-ui/.env.local` (required in dev mode because Next.js rewrites don't upgrade WebSocket connections).

### Configuration

The bot loads configuration from `config/default.toml` (checked in) and `config/local.toml` (gitignored, for overrides). All values can also be set via `PRED_AGENT_*` environment variables.

| Key | Default | Description |
|-----|---------|-------------|
| `arb_threshold` | `0.05` | Minimum model vs. market probability gap to generate a signal |
| `tick_interval_ms` | `1000` | Market polling interval in milliseconds |
| `paper_bankroll` | `10000.0` | Simulated starting capital in USD |
| `bus_capacity` | `1024` | EventBus broadcast channel buffer size |
| `target_niches` | `["crypto", "politics", "geopolitics", "macro", "sports"]` | Market categories to scan |

Live parameters (edge threshold, Kelly fraction, position limits, agent enable/disable) can be updated at runtime through the Risk Control page — no restart required.

---

## Testing

```bash
# Run all tests
cargo test --workspace

# Single crate
cargo test -p bayesian_engine

# Single test with output
cargo test -p meta_strategy test_signal_fusion -- --nocapture

# End-to-end integration (synthetic data, no external APIs)
cargo run --bin integration_harness
RUST_LOG=debug cargo run --bin integration_harness
```

### Test Coverage by Crate

| Crate | Tests |
|-------|-------|
| `world_model` | 18 |
| `scenario_engine` | 26 |
| `meta_strategy` | 25 |
| `strategy_research` | 30+ |
| `bayesian_edge_agent` | 16 |
| `graph_arb_agent` | 13 |
| `temporal_agent` | 12 |

Integration tests use a real `EventBus`; subscribers are created **before** spawning engines to eliminate subscribe-after-spawn races.

---

## Technology Stack

**Backend (Rust)**

| Crate | Purpose |
|-------|---------|
| `tokio` | Async runtime |
| `axum 0.7` | HTTP server with WebSocket support |
| `petgraph` | Market correlation graph |
| `serde` / `serde_json` | Serialisation |
| `reqwest` | HTTP client for API polling |
| `tokio-tungstenite` | WebSocket client for exchange streaming |
| `tracing` | Structured logging |
| `metrics` + `metrics-exporter-prometheus` | Prometheus metrics |
| `chrono` | Timestamps |
| `ndarray` | Numerical arrays for analytics |
| `tokio-util` | `CancellationToken` for graceful shutdown |

**Frontend (TypeScript)**

| Package | Purpose |
|---------|---------|
| `Next.js 14` | React framework |
| `Recharts` | Equity curve and chart components |
| `SWR` | REST polling with revalidation |
| `Tailwind CSS` | Utility-first styling |
| `lucide-react` | Icons |

---

## Architecture Patterns

Every engine/agent follows the same structure:

1. **`config.rs`** — typed config struct with `Default` + `validate() -> Result<(), String>`
2. **`new(config, bus) -> Self`** — validates config at construction, panics on invalid input
3. **`run(self, cancel: CancellationToken)`** — async event loop with `biased` `tokio::select!`
4. **Pure core functions** — take `&mut State`, no locks, no async; called from the event loop
5. **Integration tests** — use a real `EventBus`; subscribe `verify_rx` before spawning

**Lock discipline**: Acquire write lock → mutate state → snapshot result → drop lock — all in one scope. Never hold a lock across `.await`.

**Shared state**: `pub type SharedFoo = Arc<RwLock<FooState>>` — field is `pub` on the engine so tests can pre-seed or inspect state.

---

## Implementation Status

| Component | Status |
|-----------|--------|
| `common`, `market_graph`, `bayesian_engine` | Complete |
| `risk_engine`, `portfolio_optimizer` | Complete |
| `portfolio_engine`, `performance_analytics` | Complete |
| `world_model` | Complete (18 tests) |
| `scenario_engine` | Complete (26 tests) |
| `shock_detector` | Complete |
| `graph_arb_agent` | Complete (13 tests) |
| `temporal_agent` | Complete (12 tests) |
| `bayesian_edge_agent` | Complete (16 tests) |
| `meta_strategy` | Complete (25 tests) |
| `strategy_research` | Complete (30+ tests) |
| `execution_sim` | Complete |
| `relationship_discovery` | Complete |
| `data_ingestion` | Complete |
| `signal_priority_engine` | Complete |
| `control-panel` + `control-panel-ui` | Complete (7 dashboard pages) |
| `simulation_engine` | Placeholder — `run_monte_carlo()` returns zeroed results |
| `news_agent` | Stub — `fetch_news()` returns empty, no sentiment events emitted |
| `arb_agent` | Legacy stub — superseded by `signal_agent` + strategy agents |

---

## Disclaimer

This project is a research and simulation tool. It does **not** execute real trades. All order flow goes through `execution_sim`, which models fills with a slippage model but sends nothing to any exchange. Use of this software for real trading requires adding a real execution layer and complying with all applicable exchange terms of service and regulations.
