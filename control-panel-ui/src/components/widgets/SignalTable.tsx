'use client'

import type { SignalRecord } from '@/lib/types'
import clsx from 'clsx'

interface Props {
  signals:  SignalRecord[]
  maxRows?: number
}

function pct(n: number) { return `${(n * 100).toFixed(1)}%` }
function edge(n: number) {
  const s = (n * 100).toFixed(2)
  return n >= 0 ? `+${s}%` : `${s}%`
}
function ts(iso: string) {
  return new Date(iso).toLocaleTimeString('en-US', { hour12: false })
}

export function SignalTable({ signals, maxRows = 50 }: Props) {
  const rows = signals.slice(0, maxRows)

  return (
    <div className="overflow-x-auto">
      <table className="w-full text-xs border-collapse">
        <thead>
          <tr className="border-b border-border text-text-muted">
            <th className="text-left py-2 pr-3 font-medium">Time</th>
            <th className="text-left py-2 pr-3 font-medium">Agent</th>
            <th className="text-left py-2 pr-3 font-medium">Market</th>
            <th className="text-right py-2 pr-3 font-medium">Dir</th>
            <th className="text-right py-2 pr-3 font-medium">Model</th>
            <th className="text-right py-2 pr-3 font-medium">Market</th>
            <th className="text-right py-2 pr-3 font-medium">Edge</th>
            <th className="text-right py-2 font-medium">Conf</th>
          </tr>
        </thead>
        <tbody>
          {rows.length === 0 && (
            <tr>
              <td colSpan={8} className="py-8 text-center text-text-muted">
                No signals yet
              </td>
            </tr>
          )}
          {rows.map((s, i) => (
            <tr
              key={i}
              className="border-b border-border/50 hover:bg-bg-raised transition-colors"
            >
              <td className="py-1.5 pr-3 text-text-muted font-mono">{ts(s.timestamp)}</td>
              <td className="py-1.5 pr-3 text-accent-blue truncate max-w-[120px]">{s.agent || '—'}</td>
              <td className="py-1.5 pr-3 text-text-primary truncate max-w-[140px]">{s.market_id}</td>
              <td className={clsx(
                'py-1.5 pr-3 text-right font-semibold',
                s.direction === 'Buy'  ? 'text-accent-green' :
                s.direction === 'Sell' ? 'text-accent-red'   : 'text-accent-yellow',
              )}>
                {s.direction}
              </td>
              <td className="py-1.5 pr-3 text-right">{pct(s.model_probability)}</td>
              <td className="py-1.5 pr-3 text-right text-text-secondary">{pct(s.market_price)}</td>
              <td className={clsx(
                'py-1.5 pr-3 text-right font-semibold',
                s.edge > 0 ? 'text-accent-green' : 'text-accent-red',
              )}>
                {edge(s.edge)}
              </td>
              <td className="py-1.5 text-right text-text-secondary">{pct(s.confidence)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}
