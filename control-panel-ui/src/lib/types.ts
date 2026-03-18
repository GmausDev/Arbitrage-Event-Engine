// src/lib/types.ts — TypeScript mirror of the Rust state types.

export interface SignalRecord {
  timestamp:          string
  agent:              string
  market_id:          string
  model_probability:  number
  market_price:       number
  edge:               number
  direction:          string
  confidence:         number
  expected_value:     number
}

export interface ExecutionRecord {
  timestamp:          string
  market_id:          string
  direction:          string
  approved_fraction:  number
  fill_ratio:         number
  executed_quantity:  number
  avg_price:          number
  slippage:           number
  filled:             boolean
}

export interface MarketRecord {
  market_id:   string
  probability: number
  liquidity:   number
  bid:         number
  ask:         number
  volume:      number
  last_update: string
}

export interface PositionRecord {
  market_id:          string
  direction:          string
  size:               number
  entry_probability:  number
  opened_at:          string
}

export interface PortfolioSnapshot {
  equity:           number
  pnl:              number
  realized_pnl:     number
  unrealized_pnl:   number
  exposure:         number
  positions:        PositionRecord[]
  sharpe:           number
  max_drawdown:     number
  current_drawdown: number
  win_rate:         number
  position_count:   number
  last_update:      string
}

export interface EquityPoint {
  timestamp: string
  equity:    number
  pnl:       number
}

export interface ShockRecord {
  timestamp: string
  market_id: string
  magnitude: number
  direction: string
  source:    string
  z_score:   number
}

export interface StrategyRecord {
  strategy_id:  string
  sharpe:       number
  max_drawdown: number
  win_rate:     number
  trade_count:  number
  promoted_at:  string
}

export interface HealthResponse {
  status:        string
  uptime_secs:   number
  timestamp:     string
  event_count:   number
  market_count:  number
  paused:        boolean
  signal_buffer: number
  exec_buffer:   number
}

export interface TradingConfig {
  edge_threshold:     number
  max_position_size:  number
  kelly_fraction:     number
  signal_confidence:  number
  risk_threshold:     number
  agent_enable_flags: Record<string, boolean>
}

// ── WebSocket event union ─────────────────────────────────────────────────

export type WsEvent =
  | { type: 'Signal';              data: SignalRecord }
  | { type: 'Execution';           data: ExecutionRecord }
  | { type: 'Portfolio';           data: PortfolioSnapshot }
  | { type: 'Shock';               data: ShockRecord }
  | { type: 'MarketUpdate';        data: { market_id: string; probability: number; liquidity: number; timestamp: string } }
  | { type: 'StrategyDiscovered';  data: StrategyRecord }
  | { type: 'MetaSignal';          data: { market_id: string; direction: string; confidence: number; expected_edge: number; timestamp: string } }
  | { type: 'Heartbeat';           data: { uptime_secs: number; event_count: number; paused: boolean; market_count: number } }
