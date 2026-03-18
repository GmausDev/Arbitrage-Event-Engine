'use client'

import { useState, useCallback } from 'react'
import { MetricCard }     from '@/components/widgets/MetricCard'
import { useStrategies }  from '@/hooks/useApi'
import { useWebSocket }   from '@/hooks/useWebSocket'
import type { WsEvent, StrategyRecord } from '@/lib/types'
import { FlaskConical, Star, TrendingUp } from 'lucide-react'
import clsx from 'clsx'

function pct(n: number) { return `${(n * 100).toFixed(1)}%` }

export default function StrategyResearchPage() {
  const { data } = useStrategies()
  const [liveStrats, setLive] = useState<StrategyRecord[]>([])
  const [cycleCount, setCycles] = useState(0)

  const onEvent = useCallback((ev: WsEvent) => {
    if (ev.type === 'StrategyDiscovered') {
      setLive(prev => [ev.data, ...prev].slice(0, 50))
      setCycles(c => c + 1)
    }
  }, [])
  useWebSocket(onEvent)

  const strategies = liveStrats.length > 0 ? liveStrats : (data?.strategies ?? [])
  const best = strategies.reduce<StrategyRecord | null>((b, s) => (!b || s.sharpe > b.sharpe ? s : b), null)

  return (
    <div className="space-y-5">
      <h1 className="text-sm font-semibold text-text-primary uppercase tracking-widest">Strategy Research</h1>

      <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
        <MetricCard label="Promoted"      value={strategies.length}                highlight="blue"  icon={<FlaskConical size={13} />} />
        <MetricCard label="New This Session" value={cycleCount}                   highlight="green" icon={<TrendingUp size={13} />} />
        <MetricCard label="Best Sharpe"   value={best ? best.sharpe.toFixed(2) : '—'} highlight="green" icon={<Star size={13} />} />
        <MetricCard label="Best Win Rate" value={best ? pct(best.win_rate) : '—'} highlight="green" />
      </div>

      {/* Strategy registry */}
      <div className="bg-bg-surface border border-border rounded p-4">
        <div className="flex items-center justify-between mb-3">
          <span className="text-xs font-medium text-text-secondary uppercase tracking-wider">Strategy Registry</span>
          {liveStrats.length > 0 && (
            <span className="text-xs text-accent-green animate-pulse-slow">● LIVE</span>
          )}
        </div>

        {strategies.length === 0 ? (
          <p className="text-text-muted text-xs text-center py-10">
            No strategies promoted yet — research loop runs every 5 min.
          </p>
        ) : (
          <table className="w-full text-xs">
            <thead>
              <tr className="border-b border-border text-text-muted">
                <th className="text-left py-1.5 pr-3 font-medium">Strategy ID</th>
                <th className="text-right py-1.5 pr-3 font-medium">Sharpe</th>
                <th className="text-right py-1.5 pr-3 font-medium">Max DD</th>
                <th className="text-right py-1.5 pr-3 font-medium">Win Rate</th>
                <th className="text-right py-1.5 pr-3 font-medium">Trades</th>
                <th className="text-right py-1.5 font-medium">Promoted</th>
              </tr>
            </thead>
            <tbody>
              {strategies.map((s, i) => (
                <tr key={s.strategy_id} className="border-b border-border/40 hover:bg-bg-raised">
                  <td className="py-1.5 pr-3 text-accent-blue font-mono text-xs truncate max-w-[160px]">
                    {s.strategy_id.slice(0, 12)}…
                  </td>
                  <td className={clsx(
                    'py-1.5 pr-3 text-right font-semibold',
                    s.sharpe > 2 ? 'text-accent-green' : s.sharpe > 1 ? 'text-text-primary' : 'text-accent-yellow',
                  )}>
                    {s.sharpe.toFixed(2)}
                  </td>
                  <td className={clsx(
                    'py-1.5 pr-3 text-right',
                    s.max_drawdown > 0.1 ? 'text-accent-red' : 'text-text-secondary',
                  )}>
                    {pct(s.max_drawdown)}
                  </td>
                  <td className="py-1.5 pr-3 text-right">{pct(s.win_rate)}</td>
                  <td className="py-1.5 pr-3 text-right text-text-secondary">{s.trade_count.toLocaleString()}</td>
                  <td className="py-1.5 text-right text-text-muted">
                    {new Date(s.promoted_at).toLocaleTimeString('en-US', { hour12: false })}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>

      {/* Research config info */}
      <div className="bg-bg-surface border border-border rounded p-4 text-xs text-text-muted space-y-1">
        <div className="font-medium text-text-secondary mb-2">Research Loop Configuration</div>
        <div>Interval: every 5 min · Batch size: 200 hypotheses · Registry capacity: 50</div>
        <div>Promotion criteria: Sharpe ≥ 1.5 · Max drawdown ≤ 10% · Trades ≥ 100</div>
        <div>13 strategy archetypes: PosteriorEdge, GraphArbitrage, TemporalMomentum, ShockSignal, MarketVolatility…</div>
      </div>
    </div>
  )
}
