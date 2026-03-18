'use client'

import { useHealth } from '@/hooks/useApi'
import { Circle, Clock, Zap } from 'lucide-react'
import clsx from 'clsx'

function fmt(n: number) { return n.toLocaleString() }
function fmtUptime(s: number) {
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const sec = s % 60
  return `${h}h ${m}m ${sec}s`
}

export function TopBar() {
  const { data: health } = useHealth()

  return (
    <header className="fixed top-0 left-56 right-0 h-10 flex items-center gap-6 px-5 border-b border-border bg-bg-surface z-30">
      {/* System status */}
      <div className="flex items-center gap-1.5">
        <Circle
          size={7}
          className={clsx(
            'fill-current',
            health?.paused ? 'text-accent-yellow' : 'text-accent-green',
          )}
        />
        <span className="text-xs text-text-secondary">
          {health?.paused ? 'PAUSED' : 'RUNNING'}
        </span>
      </div>

      {/* Separator */}
      <div className="h-4 w-px bg-border" />

      {/* Event count */}
      <div className="flex items-center gap-1.5 text-xs text-text-secondary">
        <Zap size={11} className="text-accent-blue" />
        {health ? fmt(health.event_count) : '—'} events
      </div>

      {/* Markets tracked */}
      <div className="text-xs text-text-secondary">
        {health ? fmt(health.market_count) : '—'} markets
      </div>

      {/* Uptime */}
      <div className="flex items-center gap-1.5 text-xs text-text-secondary ml-auto">
        <Clock size={11} />
        {health ? fmtUptime(health.uptime_secs) : '—'}
      </div>
    </header>
  )
}
