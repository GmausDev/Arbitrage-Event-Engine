'use client'

import { useState, useCallback, useRef } from 'react'
import { ProbabilityMovement } from '@/components/charts/ProbabilityMovement'
import { useMarkets } from '@/hooks/useApi'
import { useWebSocket } from '@/hooks/useWebSocket'
import type { WsEvent } from '@/lib/types'
import { Network } from 'lucide-react'

interface PricePoint { time: string; probability: number; model?: number }
type MarketHistory = Record<string, PricePoint[]>

const MAX_HISTORY = 200

export default function MarketIntelligencePage() {
  const { data: marketsData } = useMarkets()
  const markets = marketsData?.markets ?? []

  const [selected, setSelected]   = useState<string | null>(null)
  const [history, setHistory]     = useState<MarketHistory>({})
  const [relationships, setRels]  = useState<{ a: string; b: string; strength: number }[]>([])

  const onEvent = useCallback((ev: WsEvent) => {
    if (ev.type === 'MarketUpdate') {
      const { market_id, probability, timestamp } = ev.data
      setHistory(prev => {
        const arr = prev[market_id] ?? []
        const next = [...arr, { time: timestamp, probability }].slice(-MAX_HISTORY)
        return { ...prev, [market_id]: next }
      })
    }
    if (ev.type === 'Signal') {
      // Overlay model probability on the chart
      const { market_id, model_probability, timestamp } = ev.data
      setHistory(prev => {
        const arr = prev[market_id] ?? []
        if (arr.length === 0) return prev
        const last = arr[arr.length - 1]
        const updated = { ...last, model: model_probability }
        return { ...prev, [market_id]: [...arr.slice(0, -1), updated] }
      })
    }
    if (ev.type === 'StrategyDiscovered') {
      // Relationship discovery proxy — show pairs when a new strategy arrives
    }
  }, [])

  useWebSocket(onEvent)

  const displayMarket = selected ?? markets[0]?.market_id
  const chartData     = displayMarket ? (history[displayMarket] ?? []) : []

  return (
    <div className="space-y-5">
      <h1 className="text-sm font-semibold text-text-primary uppercase tracking-widest">Market Intelligence</h1>

      <div className="grid grid-cols-1 lg:grid-cols-3 gap-4">
        {/* Market list */}
        <div className="bg-bg-surface border border-border rounded p-3 lg:col-span-1 max-h-[480px] overflow-y-auto">
          <div className="text-xs font-medium text-text-secondary uppercase tracking-wider mb-2">
            Active Markets ({markets.length})
          </div>
          {markets.length === 0 && (
            <p className="text-text-muted text-xs text-center py-8">Waiting for market data…</p>
          )}
          {markets.map(m => (
            <button
              key={m.market_id}
              onClick={() => setSelected(m.market_id)}
              className={`w-full text-left px-2.5 py-2 rounded mb-0.5 transition-colors text-xs ${
                displayMarket === m.market_id
                  ? 'bg-bg-overlay border border-border-bright text-text-primary'
                  : 'hover:bg-bg-raised text-text-secondary'
              }`}
            >
              <div className="truncate font-medium">{m.market_id}</div>
              <div className="flex justify-between mt-0.5 text-text-muted">
                <span>P: {(m.probability * 100).toFixed(1)}%</span>
                <span>Liq: ${m.liquidity.toFixed(0)}</span>
              </div>
            </button>
          ))}
        </div>

        {/* Probability movement chart */}
        <div className="bg-bg-surface border border-border rounded p-4 lg:col-span-2">
          <div className="flex items-center gap-2 mb-3">
            <Network size={13} className="text-accent-blue" />
            <span className="text-xs font-medium text-text-secondary uppercase tracking-wider">
              Probability Movement
            </span>
          </div>
          {chartData.length < 2 ? (
            <div className="flex items-center justify-center h-48 text-text-muted text-xs">
              {displayMarket ? `Waiting for data on ${displayMarket}…` : 'Select a market'}
            </div>
          ) : (
            <ProbabilityMovement data={chartData} marketId={displayMarket} height={220} />
          )}
        </div>
      </div>

      {/* Top markets by activity */}
      <div className="bg-bg-surface border border-border rounded p-4">
        <div className="text-xs font-medium text-text-secondary uppercase tracking-wider mb-3">
          Market Snapshot
        </div>
        <table className="w-full text-xs">
          <thead>
            <tr className="border-b border-border text-text-muted">
              <th className="text-left py-1.5 pr-3 font-medium">Market</th>
              <th className="text-right py-1.5 pr-3 font-medium">Bid</th>
              <th className="text-right py-1.5 pr-3 font-medium">Ask</th>
              <th className="text-right py-1.5 pr-3 font-medium">Mid</th>
              <th className="text-right py-1.5 pr-3 font-medium">Spread</th>
              <th className="text-right py-1.5 font-medium">Liquidity</th>
            </tr>
          </thead>
          <tbody>
            {markets.slice(0, 20).map(m => (
              <tr
                key={m.market_id}
                onClick={() => setSelected(m.market_id)}
                className="border-b border-border/40 hover:bg-bg-raised cursor-pointer"
              >
                <td className="py-1.5 pr-3 text-text-primary truncate max-w-[200px]">{m.market_id}</td>
                <td className="py-1.5 pr-3 text-right text-accent-green">{(m.bid * 100).toFixed(1)}¢</td>
                <td className="py-1.5 pr-3 text-right text-accent-red">{(m.ask * 100).toFixed(1)}¢</td>
                <td className="py-1.5 pr-3 text-right">{(m.probability * 100).toFixed(1)}%</td>
                <td className="py-1.5 pr-3 text-right text-text-muted">{((m.ask - m.bid) * 100).toFixed(1)}¢</td>
                <td className="py-1.5 text-right text-text-secondary">${m.liquidity.toFixed(0)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  )
}
