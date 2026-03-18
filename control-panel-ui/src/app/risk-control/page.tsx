'use client'

import { useState, useEffect } from 'react'
import { ParameterSlider } from '@/components/controls/ParameterSlider'
import { AgentToggle }     from '@/components/controls/AgentToggle'
import { MetricCard }      from '@/components/widgets/MetricCard'
import { useConfig, useHealth } from '@/hooks/useApi'
import { pauseTrading, resumeTrading, updateParameter } from '@/lib/api'
import type { TradingConfig } from '@/lib/types'
import { ShieldAlert, Play, Pause, Save, RefreshCw } from 'lucide-react'
import clsx from 'clsx'

const AGENTS = [
  'bayesian_engine',
  'temporal_agent',
  'relationship_discovery',
  'graph_arb_agent',
  'bayesian_edge_agent',
  'signal_agent',
  'meta_strategy',
]

export default function RiskControlPage() {
  const { data: cfg, mutate: refetchConfig } = useConfig()
  const { data: health, mutate: refetchHealth } = useHealth()

  const [local, setLocal]   = useState<TradingConfig | null>(null)
  const [saving, setSaving] = useState(false)
  const [status, setStatus] = useState<string | null>(null)

  // Initialise local state from server config
  useEffect(() => {
    if (cfg && !local) setLocal(cfg)
  }, [cfg, local])

  const paused = health?.paused ?? false

  async function handlePauseResume() {
    try {
      if (paused) await resumeTrading(); else await pauseTrading()
      await refetchHealth()
      setStatus(paused ? 'Trading resumed.' : 'Trading paused.')
    } catch (e) {
      setStatus(`Error: ${(e as Error).message}`)
    }
    setTimeout(() => setStatus(null), 3000)
  }

  async function handleSave() {
    if (!local) return
    setSaving(true)
    try {
      await updateParameter({
        edge_threshold:    local.edge_threshold,
        max_position_size: local.max_position_size,
        kelly_fraction:    local.kelly_fraction,
        signal_confidence: local.signal_confidence,
        risk_threshold:    local.risk_threshold,
      })
      await refetchConfig()
      setStatus('Configuration saved.')
    } catch (e) {
      setStatus(`Error: ${(e as Error).message}`)
    }
    setSaving(false)
    setTimeout(() => setStatus(null), 3000)
  }

  async function handleAgentToggle(name: string, enabled: boolean) {
    if (!local) return
    const next = { ...local, agent_enable_flags: { ...local.agent_enable_flags, [name]: enabled } }
    setLocal(next)
    try {
      await updateParameter({ agent_name: name, agent_enabled: enabled })
      setStatus(`${name} ${enabled ? 'enabled' : 'disabled'}.`)
    } catch (e) {
      setStatus(`Error: ${(e as Error).message}`)
    }
    setTimeout(() => setStatus(null), 3000)
  }

  if (!local) {
    return (
      <div className="flex items-center justify-center h-64 text-text-muted text-xs gap-2">
        <RefreshCw size={14} className="animate-spin" /> Loading configuration…
      </div>
    )
  }

  return (
    <div className="space-y-5">
      <div className="flex items-center justify-between">
        <h1 className="text-sm font-semibold text-text-primary uppercase tracking-widest">Risk Control</h1>
        <div className="flex items-center gap-2">
          {status && (
            <span className="text-xs text-accent-green">{status}</span>
          )}
          <button
            onClick={handleSave}
            disabled={saving}
            className="flex items-center gap-1.5 px-3 py-1.5 bg-accent-blue/20 border border-accent-blue/40 rounded text-xs text-accent-blue hover:bg-accent-blue/30 transition-colors disabled:opacity-50"
          >
            <Save size={11} /> {saving ? 'Saving…' : 'Save'}
          </button>
        </div>
      </div>

      {/* Pause/Resume */}
      <div className={clsx(
        'flex items-center justify-between p-4 rounded border',
        paused ? 'border-accent-yellow/40 bg-accent-yellow/5' : 'border-border bg-bg-surface',
      )}>
        <div>
          <div className="text-xs font-semibold text-text-primary mb-0.5">
            Execution Engine — {paused ? 'PAUSED' : 'RUNNING'}
          </div>
          <div className="text-xs text-text-muted">
            {paused
              ? 'Signals are being generated but no trades will be executed.'
              : 'System is actively evaluating and executing trades.'}
          </div>
        </div>
        <button
          onClick={handlePauseResume}
          className={clsx(
            'flex items-center gap-1.5 px-4 py-2 rounded border text-xs font-semibold transition-colors',
            paused
              ? 'border-accent-green/40 bg-accent-green/10 text-accent-green hover:bg-accent-green/20'
              : 'border-accent-yellow/40 bg-accent-yellow/10 text-accent-yellow hover:bg-accent-yellow/20',
          )}
        >
          {paused ? <><Play size={12} /> Resume</> : <><Pause size={12} /> Pause</>}
        </button>
      </div>

      <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
        {/* Risk parameters */}
        <div className="bg-bg-surface border border-border rounded p-5 space-y-5">
          <div className="flex items-center gap-2 mb-1">
            <ShieldAlert size={13} className="text-accent-red" />
            <span className="text-xs font-medium text-text-secondary uppercase tracking-wider">Risk Parameters</span>
          </div>
          <ParameterSlider
            label="Edge Threshold (min EV)"
            value={local.edge_threshold}
            min={0}
            max={0.1}
            step={0.001}
            format={v => `${(v * 100).toFixed(1)}%`}
            onChange={v => setLocal({ ...local, edge_threshold: v })}
          />
          <ParameterSlider
            label="Max Position Size"
            value={local.max_position_size}
            min={0.01}
            max={0.25}
            step={0.005}
            format={v => `${(v * 100).toFixed(1)}%`}
            onChange={v => setLocal({ ...local, max_position_size: v })}
          />
          <ParameterSlider
            label="Kelly Fraction"
            value={local.kelly_fraction}
            min={0.05}
            max={1.0}
            step={0.05}
            format={v => `${(v * 100).toFixed(0)}%`}
            onChange={v => setLocal({ ...local, kelly_fraction: v })}
          />
          <ParameterSlider
            label="Signal Confidence Floor"
            value={local.signal_confidence}
            min={0.0}
            max={0.9}
            step={0.01}
            format={v => `${(v * 100).toFixed(0)}%`}
            onChange={v => setLocal({ ...local, signal_confidence: v })}
          />
          <ParameterSlider
            label="Total Exposure Limit"
            value={local.risk_threshold}
            min={0.05}
            max={0.8}
            step={0.01}
            format={v => `${(v * 100).toFixed(0)}%`}
            onChange={v => setLocal({ ...local, risk_threshold: v })}
          />
        </div>

        {/* Agent toggles */}
        <div className="bg-bg-surface border border-border rounded p-5">
          <div className="text-xs font-medium text-text-secondary uppercase tracking-wider mb-4">Agent Enable Flags</div>
          <div className="space-y-2">
            {AGENTS.map(name => (
              <AgentToggle
                key={name}
                name={name}
                enabled={local.agent_enable_flags[name] ?? true}
                onChange={en => handleAgentToggle(name, en)}
              />
            ))}
          </div>
        </div>
      </div>

      {/* Current config summary */}
      <div className="grid grid-cols-2 md:grid-cols-5 gap-3">
        <MetricCard label="Edge Threshold"   value={`${(local.edge_threshold * 100).toFixed(1)}%`}    />
        <MetricCard label="Max Position"     value={`${(local.max_position_size * 100).toFixed(0)}%`} />
        <MetricCard label="Kelly Fraction"   value={`${(local.kelly_fraction * 100).toFixed(0)}%`}    />
        <MetricCard label="Conf Floor"       value={`${(local.signal_confidence * 100).toFixed(0)}%`} />
        <MetricCard label="Exposure Limit"   value={`${(local.risk_threshold * 100).toFixed(0)}%`}    />
      </div>
    </div>
  )
}
