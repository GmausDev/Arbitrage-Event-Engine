'use client'

import {
  ResponsiveContainer, LineChart, Line, XAxis, YAxis,
  CartesianGrid, Tooltip, Legend,
} from 'recharts'

interface PricePoint {
  time:        string
  probability: number
  model?:      number
}

interface Props {
  data:      PricePoint[]
  marketId?: string
  height?:   number
}

function fmtTime(t: string) {
  const d = new Date(t)
  return `${d.getHours().toString().padStart(2,'0')}:${d.getMinutes().toString().padStart(2,'0')}:${d.getSeconds().toString().padStart(2,'0')}`
}

export function ProbabilityMovement({ data, marketId, height = 200 }: Props) {
  return (
    <div>
      {marketId && (
        <div className="text-xs text-text-muted mb-2 truncate">{marketId}</div>
      )}
      <ResponsiveContainer width="100%" height={height}>
        <LineChart data={data} margin={{ top: 4, right: 4, left: 0, bottom: 0 }}>
          <CartesianGrid stroke="#1e2330" strokeDasharray="3 3" vertical={false} />
          <XAxis
            dataKey="time"
            tickFormatter={fmtTime}
            tick={{ fill: '#8892a4', fontSize: 9 }}
            axisLine={false}
            tickLine={false}
            interval="preserveStartEnd"
          />
          <YAxis
            domain={[0, 1]}
            tickFormatter={(v) => `${(v * 100).toFixed(0)}%`}
            tick={{ fill: '#8892a4', fontSize: 10 }}
            axisLine={false}
            tickLine={false}
            width={42}
          />
          <Tooltip
            contentStyle={{ background: '#181c23', border: '1px solid #1e2330', borderRadius: 6, fontSize: 11 }}
            formatter={(v: number) => `${(v * 100).toFixed(2)}%`}
          />
          <Legend
            wrapperStyle={{ fontSize: 10, color: '#8892a4' }}
          />
          <Line
            type="stepAfter"
            dataKey="probability"
            name="Market"
            stroke="#3b82f6"
            strokeWidth={1.5}
            dot={false}
          />
          <Line
            type="stepAfter"
            dataKey="model"
            name="Model"
            stroke="#10b981"
            strokeWidth={1.5}
            dot={false}
            strokeDasharray="4 2"
          />
        </LineChart>
      </ResponsiveContainer>
    </div>
  )
}
