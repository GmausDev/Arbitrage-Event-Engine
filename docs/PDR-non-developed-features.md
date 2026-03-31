# Production Design Review: Non-Developed Features

**Date:** 2026-03-31
**Scope:** Arbitrage Event Engine — 30-crate Rust workspace
**Branch:** `claude/security-production-review-4fxJs`

---

## Executive Summary

The Arbitrage Event Engine is a 30-crate Rust workspace for prediction market arbitrage across Polymarket and Kalshi. The core pipeline (data ingestion → Bayesian engine → signal agents → risk engine → execution) is functional in **simulation mode**. This PDR catalogs all features that are stubs, unimplemented, placeholder, or not production-ready, organized by severity and domain.

**Status breakdown:**
- **Fully implemented:** 22 crates (event bus, Bayesian engine, signal agents, risk engine, cost model, portfolio engine, performance analytics, meta-strategy, etc.)
- **Partially complete:** 4 crates (exchange_api, OMS, persistence, strategy_research)
- **Complete but not wired:** 2 crates (world_model, scenario_engine)
- **Stub/hardcoded:** 2 connector modules (calendar events)

---

## 1. Stub Implementations (Hardcoded / Placeholder Data)

### 1.1 Polymarket Balance & Positions — Returns Dummy Data

| Item | Detail |
|------|--------|
| **File** | `crates/exchange_api/src/connectors/polymarket.rs:204-219` |
| **What** | `get_balance()` returns hardcoded `0.0`; `get_positions()` returns `vec![]` |
| **Why** | Polymarket balance is on-chain USDC (ERC-20); positions are ERC-1155 conditional tokens. The CLOB API has no balance/position endpoints. |
| **Impact** | Account reconciliation impossible. OMS cannot verify holdings. P&L tracking relies entirely on internal state. |
| **To Fix** | Integrate Ethereum JSON-RPC or Polymarket's funding API to query wallet USDC balance. Query the Conditional Token Framework contract for ERC-1155 balances. Requires an RPC provider (Alchemy/Infura). |
| **Effort** | Medium — requires Web3/ethers-rs integration |

### 1.2 Calendar Connectors — Hardcoded Event Stubs

| Item | Detail |
|------|--------|
| **File** | `crates/data_ingestion/src/connectors/calendar_connectors.rs:20-97` |
| **What** | `PredictItCalendarConnector` returns hardcoded FOMC/election events. `EarningsCalendarConnector` returns hardcoded AAPL/NVDA earnings dates. |
| **Why** | No real economic calendar API integrated yet |
| **Impact** | Calendar-driven signals use stale dates. Temporal agent and shock detector receive incorrect event timing. |
| **To Fix** | Integrate FRED Events API, Yahoo Finance Earnings Calendar, or TradingEconomics calendar. Replace hardcoded `Vec<RawCalendarEntry>` with HTTP fetch + parse. |
| **Effort** | Low-Medium |

### 1.3 Polymarket Timestamp Parsing — Uses `Utc::now()` Placeholders

| Item | Detail |
|------|--------|
| **File** | `crates/exchange_api/src/connectors/polymarket.rs:332, 416` |
| **What** | `get_order_status()` and `get_recent_fills()` use `Utc::now()` instead of parsing the API's `created_at` field |
| **Why** | `// TODO: parse raw.created_at` |
| **Impact** | Order/fill timestamps are inaccurate. Latency tracking, P&L attribution, and trade history show wrong times. |
| **To Fix** | Parse ISO8601 strings from the CLOB API response |
| **Effort** | Low |

---

## 2. Complete Crates Not Spawned in Runtime

### 2.1 World Model

| Item | Detail |
|------|--------|
| **Crate** | `crates/world_model/` |
| **What** | Full probabilistic constraint engine: market snapshots, dependency graphs, constraint propagation, inconsistency detection. 20+ tests passing. |
| **Status** | Feature-complete, tested, **not spawned** by `prediction-agent/src/main.rs` |
| **Impact** | No probabilistic consistency enforcement. Conflicting market probabilities go undetected. |
| **To Fix** | Instantiate `WorldModel`, subscribe to EventBus, add to spawn list in main.rs |
| **Effort** | Low |

### 2.2 Scenario Engine

| Item | Detail |
|------|--------|
| **Crate** | `crates/scenario_engine/` |
| **What** | Monte Carlo scenario generation, sampling, expectation analysis. 13 tests passing. |
| **Status** | Feature-complete, tested, **not spawned** |
| **Impact** | No scenario-based risk analysis. Portfolio optimizer operates without forward-looking scenario context. |
| **To Fix** | Instantiate, subscribe, spawn — same as world_model |
| **Effort** | Low |

---

## 3. Validation Logic Defined But Not Wired

### 3.1 World Model Error Validation

| Item | Detail |
|------|--------|
| **File** | `crates/world_model/src/error.rs:5` |
| **What** | `WorldModelError` enum with `InvalidProbability`, `InvalidWeight`, `InvalidConfidence` variants defined but marked `#[allow(dead_code)]` |
| **Why** | `// TODO: wire into validation in propagate_update and WorldState::add_dependency` |
| **Impact** | Invalid probability values (outside [0,1]), negative weights, and out-of-range confidence scores silently accepted |
| **To Fix** | Call validation functions in `propagate_update()` and `add_dependency()` |
| **Effort** | Low |

### 3.2 Scenario Engine Error Validation

| Item | Detail |
|------|--------|
| **File** | `crates/scenario_engine/src/error.rs:5` |
| **What** | Same pattern — error enum defined, validation not called |
| **Impact** | Scenario sampling accepts invalid parameters silently |
| **To Fix** | Wire into `sample_scenarios()` and analysis functions |
| **Effort** | Low |

---

## 4. Missing Retry & Resilience Logic

### 4.1 Exchange API Connectors — No Retry on Failure

| Item | Detail |
|------|--------|
| **Files** | `crates/exchange_api/src/connectors/polymarket.rs`, `kalshi.rs` |
| **What** | All exchange API calls (place_order, cancel_order, get_status, get_balance, get_positions) fail immediately on network error with no retry |
| **Impact** | Transient network issues cause order failures. In live trading, this means missed fills and untracked orders. |
| **Current** | Rate-limit detection exists but uses hardcoded `1000ms` retry delay instead of parsing `Retry-After` header |
| **To Fix** | Add exponential backoff wrapper. Parse `Retry-After` headers. Distinguish transient vs permanent failures. |
| **Effort** | Medium |

### 4.2 Data Ingestion Connectors — No Retry

| Item | Detail |
|------|--------|
| **Files** | `news_connectors.rs`, `economic_connectors.rs`, `social_connectors.rs`, `market_connectors.rs` |
| **What** | HTTP failures logged then return `Ok(vec![])`. No retry attempted. |
| **Note** | `market_scanner` and `websocket_client` DO have retry with exponential backoff. Only the individual connectors lack it. |
| **Impact** | Temporary API outages cause data gaps that propagate through the pipeline |
| **To Fix** | Wrap fetch calls in retry loop or integrate with the existing `rate_limiter.rs` |
| **Effort** | Low-Medium |

---

## 5. Production Safety Gaps

### 5.1 Config Validation Panics

| Item | Detail |
|------|--------|
| **Files** | 12 engine/agent crates (see list below) |
| **What** | All engines validate config at startup with `panic!("invalid {Crate}Config: {e}")` |
| **Affected** | `portfolio_engine`, `scenario_engine`, `relationship_discovery`, `world_model`, `performance_analytics`, `shock_detector`, `simulation_engine`, `meta_strategy`, `temporal_agent`, `bayesian_edge_agent`, `graph_arb_agent`, `calibration` |
| **Impact** | Any config error crashes the entire process. In production, a single bad config field kills all 25+ running tasks. |
| **To Fix** | Return `Result<Self, ConfigError>` from constructors instead of panicking. Let main.rs decide whether to abort or skip. |
| **Effort** | Medium (touches 12 crates) |

### 5.2 Startup Panics in main.rs

| Item | Detail |
|------|--------|
| **File** | `prediction-agent/src/main.rs` |
| **Lines** | 86 (Prometheus), 245 (database), 286 (Polymarket client), 293 (Kalshi client), 381 (control panel bind) |
| **What** | `.expect()` calls crash the process on startup failures |
| **Impact** | Database file locked, port already in use, or network timeout kills the process instead of degrading gracefully |
| **To Fix** | Replace with error logging + graceful degradation where possible |
| **Effort** | Low |

### 5.3 Mutex Poisoning Risk

| Item | Detail |
|------|--------|
| **File** | `crates/persistence/src/db.rs:50` |
| **What** | `self.conn.lock().expect("database mutex poisoned")` |
| **Impact** | If any database operation panics while holding the lock, ALL subsequent database operations panic (cascading failure) |
| **To Fix** | Use `lock().map_err()` to return an error, or use `parking_lot::Mutex` which doesn't poison |
| **Effort** | Low |

---

## 6. Security Gaps

### 6.1 Control Panel Has No TLS

| Item | Detail |
|------|--------|
| **File** | `control-panel/src/server.rs` |
| **What** | Axum server listens on plain HTTP (`0.0.0.0:3001`) |
| **Impact** | Bearer tokens transmitted in cleartext. Trading controls (pause/resume) vulnerable to MITM. |
| **To Fix** | Add `axum-server` with `rustls` TLS config, or terminate TLS at reverse proxy (nginx/Caddy) |
| **Effort** | Low (if using reverse proxy) / Medium (if native TLS) |

### 6.2 `/api/metrics` Endpoint Unauthenticated

| Item | Detail |
|------|--------|
| **File** | `control-panel/src/routes/metrics_route.rs` |
| **What** | Prometheus metrics proxy endpoint has no auth middleware |
| **Impact** | Exposes algorithm performance, trade counts, order volumes, error rates to unauthenticated callers |
| **To Fix** | Add `require_auth` middleware to metrics route, or restrict to internal network |
| **Effort** | Low |

### 6.3 Auth Disabled When Env Var Not Set

| Item | Detail |
|------|--------|
| **File** | `control-panel/src/auth.rs:30-32` |
| **What** | When `CONTROL_PANEL_API_KEY` is not set, all requests pass through (dev mode) |
| **Impact** | Accidental production deploy without the env var exposes full trading controls |
| **To Fix** | Require explicit `CONTROL_PANEL_AUTH_DISABLED=true` to disable, rather than defaulting to open |
| **Effort** | Low |

### 6.4 Token in WebSocket Query Parameter

| Item | Detail |
|------|--------|
| **File** | `control-panel/src/auth.rs:49-59` |
| **What** | WebSocket auth supports `?token=...` query parameter |
| **Impact** | Query params appear in server access logs, browser history, HTTP referrer headers, and proxy logs |
| **Mitigation** | This is a common pattern for WebSocket auth (headers can't be set in browser WebSocket API). Document the risk and recommend secure proxy termination. |
| **Effort** | N/A (architectural constraint of WebSocket) |

---

## 7. Missing Test Coverage

### 7.1 Untested Critical Crates

| Crate | Tests | Risk |
|-------|-------|------|
| `common` (EventBus) | 0 | Core infrastructure — pub/sub, event ordering, backpressure |
| `oms` (Order Manager) | 0 | Critical trade path — order lifecycle, partial fills, timeouts |
| `exchange_api` | 0 integration | Exchange connectors — error paths, auth flows |
| `signal_priority_engine` | 0 | Signal routing — fast/slow path classification |
| `calibration` | 0 | Model calibration accuracy |

### 7.2 Missing Infrastructure

| Item | Status |
|------|--------|
| CI/CD (GitHub Actions) | Not configured |
| Benchmarks | None |
| Fuzz testing | None |
| Property-based testing | Minimal (~5%) |
| End-to-end integration tests | None |
| Chaos/fault injection tests | None |

---

## 8. Hardcoded Values Needing Configuration

| Location | Value | Should Be |
|----------|-------|-----------|
| `prediction-agent/src/main.rs:86` | Prometheus on `:9000` | `AppConfig.metrics_bind` |
| `polymarket.rs:124,126` | `https://clob.polymarket.com`, gamma API URL | Config or env var |
| `kalshi.rs:169` | `https://trading-api.kalshi.com/trade-api/v2` | Config or env var |
| `polymarket.rs:231` | Default order price `0.99` | Order book midpoint |
| `polymarket.rs:128-129` | Fee rate `0.5%` | Read from exchange API |
| `kalshi.rs:172,174` | Fee rate `2%`, min order `$1` | Config |
| `control-panel/src/server.rs:67` | CORS fallback `http://localhost:3000` | Config |
| `exchange_api` rate limit delays | Hardcoded `1000ms` | Parse `Retry-After` header |

---

## 9. Strategy Research — Incomplete Feedback Loop

| Item | Detail |
|------|--------|
| **File** | `crates/strategy_research/src/engine.rs:122` |
| **What** | `// Future: seed live market snapshots for hypothesis refinement` |
| **Status** | Strategy research generates hypotheses and backtests them, but does NOT consume live market state to adapt hypothesis generation |
| **Impact** | Hypothesis templates are static; research engine can't learn from current market conditions |
| **To Fix** | Subscribe to `Event::Market` / `Event::Portfolio` to seed hypothesis parameters from live data |
| **Effort** | Medium |

---

## 10. Priority Roadmap

### P0 — Required for Live Trading
1. Implement Polymarket balance/position queries (on-chain integration)
2. Add retry logic to exchange API connectors
3. Fix Polymarket timestamp parsing (2 TODOs)
4. Replace config `panic!()` with `Result` returns in 12 crates
5. Require explicit opt-in to disable auth (not default-open)

### P1 — Required for Production Reliability
6. Wire world_model and scenario_engine into runtime
7. Wire validation logic in world_model and scenario_engine error types
8. Add TLS termination for control panel
9. Authenticate `/api/metrics` endpoint
10. Add OMS and EventBus test suites
11. Set up CI/CD pipeline

### P2 — Operational Excellence
12. Replace hardcoded exchange URLs/fees with config
13. Parse `Retry-After` headers for rate-limit handling
14. Add retry logic to data ingestion connectors
15. Replace calendar connector stubs with real APIs
16. Add benchmarks for hot paths (Bayesian engine, cost model)
17. Strategy research live-market feedback loop

### P3 — Hardening
18. Replace `parking_lot::Mutex` or handle mutex poisoning gracefully
19. Add fuzz testing for parsers
20. Add end-to-end integration test harness
21. Property-based tests for numeric calculations
22. Chaos testing (exchange failures, network partitions)
