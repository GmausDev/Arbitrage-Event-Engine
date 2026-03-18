'use client'

import { useState, useCallback } from 'react'
import { MetricCard }      from '@/components/widgets/MetricCard'
import { MarketUpdateRate } from '@/components/charts/MarketUpdateRate'
import { useHealth, useMarkets } from '@/hooks/useApi'
import { useWebSocket }    from '@/hooks/useWebSocket'
import type { WsEvent, ShockRecord } from '@/lib/types'
import { Activity, Database, Radio, TrendingUp, Zap } from 'lucide-react'

interface RatePoint { time: string; updates_per_sec: number }

function now() { return new Date().toISOString() }

export default function OverviewPage() {
  const { data: health }  = useHealth()
  const { data: markets } = useMarkets()

  const [updateRate, setUpdateRate]  = useState<RatePoint[]>([])
  const [recentShocks, setShocks]    = useState<ShockRecord[]>([])
  const [signalCount, setSignalCount] = useState(0)
  const [marketTick, setMarketTick]  = useState(0)

  // Rolling 1-second market-update rate tracker
  const [tickBuf, setTickBuf] = useState(0)

  const onEvent = useCallback((ev: WsEvent) => {
    if (ev.type === 'MarketUpdate') {
      setMarketTick(t => t + 1)
      setTickBuf(b => b + 1)
    }
    if (ev.type === 'Signal') {
      setSignalCount(c => c + 1)
    }
    if (ev.type === 'Shock') {
      setShocks(prev => [ev.data, ...prev].slice(0, 10))
      setUpdateRate(prev => {
        const pt = { time: now(), updates_per_sec: tickBuf }
        setTickBuf(0)
        return [...prev.slice(-60), pt]
      })
    }
    if (ev.type === 'Heartbeat') {
      setUpdateRate(prev => {
        const pt = { time: now(), updates_per_sec: tickBuf }
        setTickBuf(0)
        return [...prev.slice(-60), pt]
      })
    }
  }, [tickBuf])

  useWebSocket(onEvent)

  return (
    <div className="space-y-5">
      <h1 className="text-sm font-semibold text-text-primary uppercase tracking-widest">System Overview</h1>

      {/* KPI Row */}
      <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
        <MetricCard
          label="Status"
          value={health?.paused ? 'PAUSED' : 'RUNNING'}
          highlight={health?.paused ? 'yellow' : 'green'}
          icon={<Activity size={13} />}
        />
        <MetricCard
          label="Total Events"
          value={health?.event_count?.toLocaleString() ?? '—'}
          highlight="blue"
          icon={<Zap size={13} />}
        />
        <MetricCard
          label="Active Markets"
          value={health?.market_count ?? markets?.count ?? '—'}
          highlight="blue"
          icon={<Database size={13} />}
        />
        <MetricCard
          label="Signals Seen"
          value={signalCount.toLocaleString()}
          highlight="green"
          icon={<TrendingUp size={13} />}
        />
      </div>

      {/* Market update rate chart */}
      <div className="bg-bg-surface border border-border rounded p-4">
        <div className="flex items-center gap-2 mb-3">
          <Radio size={13} className="text-accent-blue" />
          <span className="text-xs font-medium text-text-secondary uppercase tracking-wider">Market Update Rate</span>
        </div>
        <MarketUpdateRate data={updateRate} />
      </div>

      {/* Recent shocks */}
      <div className="bg-bg-surface border border-border rounded p-4">
        <div className="text-xs font-medium text-text-secondary uppercase tracking-wider mb-3">Recent Shocks</div>
        {recentShocks.length === 0 ? (
          <p className="text-text-muted text-xs py-4 text-center">No shocks detected</p>
        ) : (
          <table className="w-full text-xs">
            <thead>
              <tr className="border-b border-border text-text-muted">
                <th className="text-left py-1.5 pr-3 font-medium">Market</th>
                <th className="text-right py-1.5 pr-3 font-medium">Magnitude</th>
                <th className="text-right py-1.5 pr-3 font-medium">Direction</th>
                <th className="text-right py-1.5 pr-3 font-medium">Z-Score</th>
                <th className="text-right py-1.5 font-medium">Source</th>
              </tr>
            </thead>
            <tbody>
              {recentShocks.map((s, i) => (
                <tr key={i} className="border-b border-border/40 hover:bg-bg-raised">
                  <td className="py-1.5 pr-3 text-text-primary truncate max-w-[160px]">{s.market_id}</td>
                  <td className="py-1.5 pr-3 text-right text-accent-yellow">{(s.magnitude * 100).toFixed(1)}%</td>
                  <td className="py-1.5 pr-3 text-right text-text-secondary">{s.direction}</td>
                  <td className="py-1.5 pr-3 text-right">{s.z_score.toFixed(2)}σ</td>
                  <td className="py-1.5 text-right text-text-muted">{s.source}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>

      {/* Uptime */}
      <div className="text-xs text-text-muted">
        Uptime: {health ? `${Math.floor(health.uptime_secs / 3600)}h ${Math.floor((health.uptime_secs % 3600) / 60)}m` : '—'}
        {' · '}Signal buffer: {health?.signal_buffer ?? '—'}
        {' · '}Exec buffer: {health?.exec_buffer ?? '—'}
      </div>
    </div>
  )
}
