'use client'

import type { ExecutionRecord } from '@/lib/types'
import clsx from 'clsx'

interface Props {
  executions: ExecutionRecord[]
  maxRows?:   number
}

function pct(n: number)  { return `${(n * 100).toFixed(2)}%` }
function bps(n: number)  { return `${(n * 10_000).toFixed(1)} bps` }
function ts(iso: string) { return new Date(iso).toLocaleTimeString('en-US', { hour12: false }) }

export function ExecutionTable({ executions, maxRows = 50 }: Props) {
  const rows = executions.slice(0, maxRows)
  return (
    <div className="overflow-x-auto">
      <table className="w-full text-xs border-collapse">
        <thead>
          <tr className="border-b border-border text-text-muted">
            <th className="text-left py-2 pr-3 font-medium">Time</th>
            <th className="text-left py-2 pr-3 font-medium">Market</th>
            <th className="text-right py-2 pr-3 font-medium">Dir</th>
            <th className="text-right py-2 pr-3 font-medium">Fill</th>
            <th className="text-right py-2 pr-3 font-medium">Qty</th>
            <th className="text-right py-2 pr-3 font-medium">Avg Price</th>
            <th className="text-right py-2 pr-3 font-medium">Slippage</th>
            <th className="text-right py-2 font-medium">Status</th>
          </tr>
        </thead>
        <tbody>
          {rows.length === 0 && (
            <tr>
              <td colSpan={8} className="py-8 text-center text-text-muted">No executions yet</td>
            </tr>
          )}
          {rows.map((e, i) => (
            <tr key={i} className="border-b border-border/50 hover:bg-bg-raised transition-colors">
              <td className="py-1.5 pr-3 text-text-muted">{ts(e.timestamp)}</td>
              <td className="py-1.5 pr-3 text-text-primary truncate max-w-[140px]">{e.market_id}</td>
              <td className={clsx(
                'py-1.5 pr-3 text-right font-semibold',
                e.direction === 'Buy' ? 'text-accent-green' : 'text-accent-red',
              )}>
                {e.direction}
              </td>
              <td className="py-1.5 pr-3 text-right">{pct(e.fill_ratio)}</td>
              <td className="py-1.5 pr-3 text-right">{pct(e.executed_quantity)}</td>
              <td className="py-1.5 pr-3 text-right">{pct(e.avg_price)}</td>
              <td className={clsx(
                'py-1.5 pr-3 text-right',
                Math.abs(e.slippage) > 0.005 ? 'text-accent-red' : 'text-text-secondary',
              )}>
                {bps(Math.abs(e.slippage))}
              </td>
              <td className="py-1.5 text-right">
                <span className={clsx(
                  'px-1.5 py-0.5 rounded text-xs',
                  e.filled ? 'bg-accent-green/20 text-accent-green' : 'bg-accent-red/20 text-accent-red',
                )}>
                  {e.filled ? 'FILLED' : 'MISS'}
                </span>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}
