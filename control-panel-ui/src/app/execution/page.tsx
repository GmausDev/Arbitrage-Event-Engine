'use client'

import { useState, useCallback } from 'react'
import { ExecutionTable } from '@/components/widgets/ExecutionTable'
import { MetricCard }     from '@/components/widgets/MetricCard'
import { useExecutions }  from '@/hooks/useApi'
import { useWebSocket }   from '@/hooks/useWebSocket'
import type { WsEvent, ExecutionRecord } from '@/lib/types'
import { Zap, CheckCircle, XCircle } from 'lucide-react'

function bps(n: number) { return `${(n * 10_000).toFixed(1)} bps` }

export default function ExecutionPage() {
  const { data: polled } = useExecutions()
  const [live, setLive]  = useState<ExecutionRecord[]>([])

  const onEvent = useCallback((ev: WsEvent) => {
    if (ev.type === 'Execution') {
      setLive(prev => [ev.data, ...prev].slice(0, 200))
    }
  }, [])
  useWebSocket(onEvent)

  const execs  = live.length > 0 ? live : (polled?.executions ?? [])
  const filled = execs.filter(e => e.filled)
  const misses = execs.filter(e => !e.filled)
  const avgSlip = filled.length > 0
    ? filled.reduce((a, e) => a + Math.abs(e.slippage), 0) / filled.length
    : 0
  const fillRate = execs.length > 0 ? filled.length / execs.length : 0

  return (
    <div className="space-y-5">
      <h1 className="text-sm font-semibold text-text-primary uppercase tracking-widest">Execution Monitor</h1>

      <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
        <MetricCard label="Total Executions" value={execs.length}              highlight="blue"  icon={<Zap size={13} />} />
        <MetricCard label="Filled"           value={filled.length} trend="up"  highlight="green" icon={<CheckCircle size={13} />} />
        <MetricCard label="Missed"           value={misses.length} trend="down" highlight="red"  icon={<XCircle size={13} />} />
        <MetricCard
          label="Fill Rate"
          value={`${(fillRate * 100).toFixed(1)}%`}
          highlight={fillRate > 0.8 ? 'green' : fillRate > 0.5 ? 'yellow' : 'red'}
        />
      </div>

      <div className="grid grid-cols-2 gap-3">
        <MetricCard
          label="Avg Slippage"
          value={bps(avgSlip)}
          highlight={avgSlip > 0.005 ? 'red' : 'green'}
          sub="filled trades only"
        />
        <MetricCard
          label="Avg Fill Ratio"
          value={filled.length > 0
            ? `${(filled.reduce((a, e) => a + e.fill_ratio, 0) / filled.length * 100).toFixed(1)}%`
            : '—'}
        />
      </div>

      <div className="bg-bg-surface border border-border rounded p-4">
        <div className="flex items-center justify-between mb-3">
          <span className="text-xs font-medium text-text-secondary uppercase tracking-wider">Recent Executions</span>
          {live.length > 0 && (
            <span className="text-xs text-accent-green animate-pulse-slow">● LIVE</span>
          )}
        </div>
        <ExecutionTable executions={execs} maxRows={100} />
      </div>
    </div>
  )
}
