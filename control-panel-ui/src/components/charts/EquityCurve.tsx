'use client'

import {
  ResponsiveContainer, AreaChart, Area, XAxis, YAxis,
  CartesianGrid, Tooltip, ReferenceLine,
} from 'recharts'
import type { EquityPoint } from '@/lib/types'

interface Props {
  data:      EquityPoint[]
  baseline?: number   // initial bankroll for zero-line
  height?:   number
}

function fmtTime(iso: string) {
  const d = new Date(iso)
  return `${d.getHours().toString().padStart(2,'0')}:${d.getMinutes().toString().padStart(2,'0')}`
}

function fmtUSD(n: number) { return `$${n.toFixed(0)}` }

const CustomTooltip = ({ active, payload, label }: any) => {
  if (!active || !payload?.length) return null
  const d = payload[0].payload as EquityPoint
  return (
    <div className="bg-bg-raised border border-border rounded px-3 py-2 text-xs">
      <div className="text-text-muted mb-1">{fmtTime(d.timestamp)}</div>
      <div className="text-text-primary">Equity: <span className="text-accent-green font-semibold">{fmtUSD(d.equity)}</span></div>
      <div className="text-text-secondary">PnL: <span className={d.pnl >= 0 ? 'text-accent-green' : 'text-accent-red'}>{d.pnl >= 0 ? '+' : ''}{fmtUSD(d.pnl)}</span></div>
    </div>
  )
}

export function EquityCurve({ data, baseline = 10_000, height = 220 }: Props) {
  const gradId = 'equity-gradient'
  return (
    <ResponsiveContainer width="100%" height={height}>
      <AreaChart data={data} margin={{ top: 4, right: 4, left: 0, bottom: 0 }}>
        <defs>
          <linearGradient id={gradId} x1="0" y1="0" x2="0" y2="1">
            <stop offset="5%"  stopColor="#10b981" stopOpacity={0.25} />
            <stop offset="95%" stopColor="#10b981" stopOpacity={0}    />
          </linearGradient>
        </defs>
        <CartesianGrid stroke="#1e2330" strokeDasharray="3 3" vertical={false} />
        <XAxis
          dataKey="timestamp"
          tickFormatter={fmtTime}
          tick={{ fill: '#8892a4', fontSize: 10 }}
          axisLine={false}
          tickLine={false}
          interval="preserveStartEnd"
        />
        <YAxis
          tickFormatter={fmtUSD}
          tick={{ fill: '#8892a4', fontSize: 10 }}
          axisLine={false}
          tickLine={false}
          width={60}
        />
        <Tooltip content={<CustomTooltip />} />
        <ReferenceLine y={baseline} stroke="#1e2330" strokeDasharray="4 4" />
        <Area
          type="monotone"
          dataKey="equity"
          stroke="#10b981"
          strokeWidth={1.5}
          fill={`url(#${gradId})`}
          dot={false}
          activeDot={{ r: 3, fill: '#10b981' }}
        />
      </AreaChart>
    </ResponsiveContainer>
  )
}
