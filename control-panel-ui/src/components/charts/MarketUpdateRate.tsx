'use client'

import { ResponsiveContainer, AreaChart, Area, XAxis, YAxis, CartesianGrid, Tooltip } from 'recharts'

interface RatePoint { time: string; updates_per_sec: number }

interface Props {
  data:   RatePoint[]
  height?: number
}

export function MarketUpdateRate({ data, height = 140 }: Props) {
  const gradId = 'rate-gradient'
  return (
    <ResponsiveContainer width="100%" height={height}>
      <AreaChart data={data} margin={{ top: 4, right: 4, left: 0, bottom: 0 }}>
        <defs>
          <linearGradient id={gradId} x1="0" y1="0" x2="0" y2="1">
            <stop offset="5%"  stopColor="#3b82f6" stopOpacity={0.3} />
            <stop offset="95%" stopColor="#3b82f6" stopOpacity={0}   />
          </linearGradient>
        </defs>
        <CartesianGrid stroke="#1e2330" strokeDasharray="3 3" vertical={false} />
        <XAxis dataKey="time" tick={{ fill: '#8892a4', fontSize: 9 }} axisLine={false} tickLine={false} interval="preserveStartEnd" />
        <YAxis tick={{ fill: '#8892a4', fontSize: 10 }} axisLine={false} tickLine={false} width={32} />
        <Tooltip
          contentStyle={{ background: '#181c23', border: '1px solid #1e2330', borderRadius: 6, fontSize: 11 }}
          formatter={(v: number) => [`${v.toFixed(1)} /s`, 'Updates']}
        />
        <Area type="monotone" dataKey="updates_per_sec" stroke="#3b82f6" strokeWidth={1.5} fill={`url(#${gradId})`} dot={false} />
      </AreaChart>
    </ResponsiveContainer>
  )
}
