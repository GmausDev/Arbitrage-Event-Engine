// src/lib/api.ts — typed fetch helpers for the Rust backend.

import type {
  EquityPoint,
  ExecutionRecord,
  HealthResponse,
  MarketRecord,
  PortfolioSnapshot,
  SignalRecord,
  ShockRecord,
  StrategyRecord,
  TradingConfig,
} from './types'

const BASE = '/api'

async function get<T>(path: string): Promise<T> {
  const res = await fetch(`${BASE}${path}`, { cache: 'no-store' })
  if (!res.ok) throw new Error(`GET ${path} → ${res.status}`)
  return res.json()
}

async function post<T>(path: string, body?: unknown): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    method:  'POST',
    headers: { 'Content-Type': 'application/json' },
    body:    body !== undefined ? JSON.stringify(body) : undefined,
  })
  if (!res.ok) throw new Error(`POST ${path} → ${res.status}`)
  return res.json()
}

// ── System ────────────────────────────────────────────────────────────────

export const fetchHealth  = (): Promise<HealthResponse> => get('/system/health')
export const fetchMetrics = (): Promise<string>         => fetch(`${BASE}/metrics`).then(r => r.text())

// ── Portfolio ─────────────────────────────────────────────────────────────

export const fetchPortfolio   = (): Promise<PortfolioSnapshot>           => get('/portfolio')
export const fetchEquityCurve = (): Promise<{ points: EquityPoint[] }>   => get('/portfolio/equity_curve')

// ── Signals ───────────────────────────────────────────────────────────────

export const fetchSignals  = (limit = 100): Promise<{ signals: SignalRecord[]; total: number }> =>
  get(`/signals/recent?limit=${limit}`)
export const fetchShocks   = (limit = 50):  Promise<{ shocks: ShockRecord[];   total: number }> =>
  get(`/signals/shocks?limit=${limit}`)

// ── Markets ───────────────────────────────────────────────────────────────

export const fetchMarkets    = (): Promise<{ markets: MarketRecord[];     count: number }> => get('/markets/active')
export const fetchStrategies = (): Promise<{ strategies: StrategyRecord[]; count: number }> => get('/strategies')

// ── Executions ────────────────────────────────────────────────────────────

export const fetchExecutions = (): Promise<{ executions: ExecutionRecord[]; total: number }> => get('/executions')

// ── Control ───────────────────────────────────────────────────────────────

export const fetchConfig        = (): Promise<TradingConfig>  => get('/control/config')
export const pauseTrading       = (): Promise<{ ok: boolean }> => post('/control/pause_trading')
export const resumeTrading      = (): Promise<{ ok: boolean }> => post('/control/resume_trading')
export const updateParameter    = (patch: Partial<TradingConfig> & { agent_name?: string; agent_enabled?: boolean }): Promise<{ ok: boolean }> =>
  post('/control/update_parameter', patch)
