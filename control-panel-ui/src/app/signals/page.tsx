'use client'

import { useState, useCallback } from 'react'
import { SignalTable }           from '@/components/widgets/SignalTable'
import { SignalEdgeDistribution } from '@/components/charts/SignalEdgeDistribution'
import { MetricCard }            from '@/components/widgets/MetricCard'
import { useSignals, useShocks } from '@/hooks/useApi'
import { useWebSocket }          from '@/hooks/useWebSocket'
import type { WsEvent, SignalRecord } from '@/lib/types'
import { Activity, AlertTriangle, TrendingUp } from 'lucide-react'

export default function SignalsPage() {
  const { data: sigData }   = useSignals(200)
  const { data: shockData } = useShocks(20)

  const [liveSignals, setLive] = useState<SignalRecord[]>([])

  const onEvent = useCallback((ev: WsEvent) => {
    if (ev.type === 'Signal') {
      setLive(prev => [ev.data, ...prev].slice(0, 200))
    }
  }, [])
  useWebSocket(onEvent)

  // Merge live signals with polled data (live takes priority)
  const allSignals = liveSignals.length > 0 ? liveSignals : (sigData?.signals ?? [])

  const buys  = allSignals.filter(s => s.direction === 'Buy').length
  const sells = allSignals.filter(s => s.direction === 'Sell').length
  const avgEdge = allSignals.length > 0
    ? allSignals.reduce((a, s) => a + s.edge, 0) / allSignals.length
    : 0

  return (
    <div className="space-y-5">
      <h1 className="text-sm font-semibold text-text-primary uppercase tracking-widest">Signal Monitor</h1>

      {/* KPIs */}
      <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
        <MetricCard label="Total Signals"  value={allSignals.length}            highlight="blue"   icon={<Activity size={13} />} />
        <MetricCard label="Buy Signals"    value={buys}   trend="up"            highlight="green"  icon={<TrendingUp size={13} />} />
        <MetricCard label="Sell Signals"   value={sells}  trend="down"          highlight="red"    icon={<TrendingUp size={13} className="rotate-180" />} />
        <MetricCard
          label="Avg Edge"
          value={`${(avgEdge * 100).toFixed(2)}%`}
          highlight={avgEdge > 0 ? 'green' : 'red'}
        />
      </div>

      {/* Edge distribution */}
      <div className="bg-bg-surface border border-border rounded p-4">
        <div className="text-xs font-medium text-text-secondary uppercase tracking-wider mb-3">
          Edge Distribution
        </div>
        <SignalEdgeDistribution signals={allSignals} height={160} />
      </div>

      {/* Live signal table */}
      <div className="bg-bg-surface border border-border rounded p-4">
        <div className="flex items-center justify-between mb-3">
          <span className="text-xs font-medium text-text-secondary uppercase tracking-wider">Live Signals</span>
          {liveSignals.length > 0 && (
            <span className="text-xs text-accent-green animate-pulse-slow">● LIVE</span>
          )}
        </div>
        <SignalTable signals={allSignals} maxRows={100} />
      </div>

      {/* Shocks panel */}
      {shockData && shockData.shocks.length > 0 && (
        <div className="bg-bg-surface border border-border rounded p-4">
          <div className="flex items-center gap-2 mb-3">
            <AlertTriangle size={13} className="text-accent-yellow" />
            <span className="text-xs font-medium text-text-secondary uppercase tracking-wider">
              Recent Shocks ({shockData.shocks.length})
            </span>
          </div>
          <table className="w-full text-xs">
            <thead>
              <tr className="border-b border-border text-text-muted">
                <th className="text-left py-1.5 pr-3 font-medium">Market</th>
                <th className="text-right py-1.5 pr-3 font-medium">Magnitude</th>
                <th className="text-right py-1.5 pr-3 font-medium">Z-Score</th>
                <th className="text-right py-1.5 pr-3 font-medium">Dir</th>
                <th className="text-right py-1.5 font-medium">Source</th>
              </tr>
            </thead>
            <tbody>
              {shockData.shocks.map((s, i) => (
                <tr key={i} className="border-b border-border/40 hover:bg-bg-raised">
                  <td className="py-1.5 pr-3 text-text-primary truncate max-w-[180px]">{s.market_id}</td>
                  <td className="py-1.5 pr-3 text-right text-accent-yellow">{(s.magnitude * 100).toFixed(1)}%</td>
                  <td className="py-1.5 pr-3 text-right">{s.z_score.toFixed(2)}σ</td>
                  <td className="py-1.5 pr-3 text-right text-text-secondary">{s.direction}</td>
                  <td className="py-1.5 text-right text-text-muted">{s.source}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  )
}
