import clsx from 'clsx'
import type { ReactNode } from 'react'

interface Props {
  label:      string
  value:      string | number
  sub?:       string
  trend?:     'up' | 'down' | 'neutral'
  highlight?: 'green' | 'red' | 'blue' | 'yellow'
  icon?:      ReactNode
  className?: string
}

export function MetricCard({ label, value, sub, trend, highlight, icon, className }: Props) {
  const trendColor =
    trend === 'up'   ? 'text-accent-green' :
    trend === 'down' ? 'text-accent-red'   : 'text-text-secondary'

  const hlColor =
    highlight === 'green'  ? 'border-l-accent-green' :
    highlight === 'red'    ? 'border-l-accent-red'   :
    highlight === 'blue'   ? 'border-l-accent-blue'  :
    highlight === 'yellow' ? 'border-l-accent-yellow': ''

  return (
    <div
      className={clsx(
        'bg-bg-surface border border-border rounded p-4 border-l-2',
        hlColor || 'border-l-border',
        className,
      )}
    >
      <div className="flex items-center justify-between mb-1">
        <span className="text-text-muted text-xs uppercase tracking-wider">{label}</span>
        {icon && <span className="text-text-muted">{icon}</span>}
      </div>
      <div className={clsx('text-xl font-semibold', trendColor || 'text-text-primary')}>
        {typeof value === 'number' ? value.toLocaleString() : value}
      </div>
      {sub && <div className="text-text-muted text-xs mt-0.5">{sub}</div>}
    </div>
  )
}
