'use client'

import { useState, useCallback } from 'react'
import { EquityCurve }   from '@/components/charts/EquityCurve'
import { MetricCard }    from '@/components/widgets/MetricCard'
import { usePortfolio, useEquityCurve } from '@/hooks/useApi'
import { useWebSocket }  from '@/hooks/useWebSocket'
import type { WsEvent, PortfolioSnapshot, EquityPoint } from '@/lib/types'
import { BarChart2, TrendingDown, TrendingUp } from 'lucide-react'
import clsx from 'clsx'

function pct(n: number) { return `${(n * 100).toFixed(2)}%` }
function usd(n: number) { return `$${n.toFixed(2)}` }
function sign(n: number) { return n >= 0 ? `+${usd(n)}` : usd(n) }

export default function PortfolioPage() {
  const { data: polled }     = usePortfolio()
  const { data: curveData }  = useEquityCurve()

  const [live, setLive]     = useState<PortfolioSnapshot | null>(null)
  const [liveCurve, setLiveCurve] = useState<EquityPoint[]>([])

  const onEvent = useCallback((ev: WsEvent) => {
    if (ev.type === 'Portfolio') {
      setLive(ev.data)
      setLiveCurve(prev => [...prev.slice(-999), { timestamp: ev.data.last_update, equity: ev.data.equity, pnl: ev.data.pnl }])
    }
  }, [])
  useWebSocket(onEvent)

  const p = live ?? polled
  const curve = liveCurve.length > 0 ? liveCurve : (curveData?.points ?? [])

  return (
    <div className="space-y-5">
      <h1 className="text-sm font-semibold text-text-primary uppercase tracking-widest">Portfolio Dashboard</h1>

      {/* KPIs */}
      <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
        <MetricCard
          label="Equity"
          value={p ? usd(p.equity) : '—'}
          highlight="blue"
          icon={<BarChart2 size={13} />}
        />
        <MetricCard
          label="PnL"
          value={p ? sign(p.pnl) : '—'}
          trend={p && p.pnl > 0 ? 'up' : p && p.pnl < 0 ? 'down' : 'neutral'}
          highlight={p && p.pnl >= 0 ? 'green' : 'red'}
          icon={<TrendingUp size={13} />}
        />
        <MetricCard
          label="Drawdown"
          value={p ? pct(p.current_drawdown) : '—'}
          sub={`Max: ${p ? pct(p.max_drawdown) : '—'}`}
          highlight={p && p.current_drawdown > 0.05 ? 'red' : 'yellow'}
          icon={<TrendingDown size={13} />}
        />
        <MetricCard
          label="Sharpe"
          value={p ? p.sharpe.toFixed(2) : '—'}
          highlight={p && p.sharpe > 1 ? 'green' : p && p.sharpe < 0 ? 'red' : 'yellow'}
        />
      </div>

      <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
        <MetricCard label="Realized PnL"   value={p ? sign(p.realized_pnl) : '—'}   highlight={p && p.realized_pnl >= 0 ? 'green' : 'red'} />
        <MetricCard label="Unrealized PnL" value={p ? sign(p.unrealized_pnl) : '—'} highlight={p && p.unrealized_pnl >= 0 ? 'green' : 'red'} />
        <MetricCard label="Exposure"       value={p ? pct(p.exposure / (p.equity || 1)) : '—'} sub={p ? usd(p.exposure) : undefined} />
        <MetricCard label="Positions"      value={p?.position_count ?? '—'} />
        <MetricCard label="Win Rate"       value={p ? pct(p.win_rate) : '—'} highlight={p && p.win_rate > 0.5 ? 'green' : 'red'} />
      </div>

      {/* Equity curve */}
      <div className="bg-bg-surface border border-border rounded p-4">
        <div className="flex items-center justify-between mb-3">
          <span className="text-xs font-medium text-text-secondary uppercase tracking-wider">Equity Curve</span>
          {live && <span className="text-xs text-accent-green animate-pulse-slow">● LIVE</span>}
        </div>
        {curve.length < 2 ? (
          <div className="flex items-center justify-center h-52 text-text-muted text-xs">Waiting for portfolio updates…</div>
        ) : (
          <EquityCurve data={curve} height={220} />
        )}
      </div>

      {/* Positions table */}
      <div className="bg-bg-surface border border-border rounded p-4">
        <div className="text-xs font-medium text-text-secondary uppercase tracking-wider mb-3">
          Open Positions ({p?.position_count ?? 0})
        </div>
        {(!p || p.positions.length === 0) ? (
          <p className="text-text-muted text-xs text-center py-6">No open positions</p>
        ) : (
          <table className="w-full text-xs">
            <thead>
              <tr className="border-b border-border text-text-muted">
                <th className="text-left py-1.5 pr-3 font-medium">Market</th>
                <th className="text-right py-1.5 pr-3 font-medium">Dir</th>
                <th className="text-right py-1.5 pr-3 font-medium">Size</th>
                <th className="text-right py-1.5 font-medium">Entry</th>
              </tr>
            </thead>
            <tbody>
              {p.positions.map((pos, i) => (
                <tr key={i} className="border-b border-border/40 hover:bg-bg-raised">
                  <td className="py-1.5 pr-3 text-text-primary truncate max-w-[200px]">{pos.market_id}</td>
                  <td className={clsx(
                    'py-1.5 pr-3 text-right font-semibold',
                    pos.direction === 'Buy' ? 'text-accent-green' : 'text-accent-red',
                  )}>
                    {pos.direction}
                  </td>
                  <td className="py-1.5 pr-3 text-right">{pct(pos.size)}</td>
                  <td className="py-1.5 text-right text-text-secondary">{pct(pos.entry_probability)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  )
}
