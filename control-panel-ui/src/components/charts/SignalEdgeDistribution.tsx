'use client'

import {
  ResponsiveContainer, BarChart, Bar, XAxis, YAxis,
  CartesianGrid, Tooltip, Cell,
} from 'recharts'
import type { SignalRecord } from '@/lib/types'

interface Props {
  signals: SignalRecord[]
  height?: number
}

function buildHistogram(signals: SignalRecord[], bins = 20) {
  if (signals.length === 0) return []
  const edges = signals.map(s => s.edge)
  const min = Math.min(...edges)
  const max = Math.max(...edges)
  const range = max - min || 0.01
  const step  = range / bins
  const hist: { bucket: string; count: number; center: number }[] = []

  for (let i = 0; i < bins; i++) {
    const lo = min + i * step
    const hi = lo + step
    const count = edges.filter(e => e >= lo && e < hi).length
    hist.push({
      bucket: `${(lo * 100).toFixed(1)}%`,
      count,
      center: (lo + hi) / 2,
    })
  }
  return hist
}

export function SignalEdgeDistribution({ signals, height = 180 }: Props) {
  const data = buildHistogram(signals)
  return (
    <ResponsiveContainer width="100%" height={height}>
      <BarChart data={data} margin={{ top: 4, right: 4, left: 0, bottom: 0 }}>
        <CartesianGrid stroke="#1e2330" strokeDasharray="3 3" vertical={false} />
        <XAxis
          dataKey="bucket"
          tick={{ fill: '#8892a4', fontSize: 9 }}
          axisLine={false}
          tickLine={false}
          interval={3}
        />
        <YAxis
          tick={{ fill: '#8892a4', fontSize: 10 }}
          axisLine={false}
          tickLine={false}
          width={30}
        />
        <Tooltip
          contentStyle={{ background: '#181c23', border: '1px solid #1e2330', borderRadius: 6, fontSize: 11 }}
          labelStyle={{ color: '#e2e8f0' }}
          itemStyle={{ color: '#8892a4' }}
        />
        <Bar dataKey="count" radius={[2, 2, 0, 0]}>
          {data.map((d, i) => (
            <Cell key={i} fill={d.center > 0 ? '#10b981' : '#ef4444'} fillOpacity={0.75} />
          ))}
        </Bar>
      </BarChart>
    </ResponsiveContainer>
  )
}
