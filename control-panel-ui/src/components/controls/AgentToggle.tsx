'use client'

import clsx from 'clsx'

interface Props {
  name:     string
  enabled:  boolean
  onChange: (enabled: boolean) => void
  loading?: boolean
}

export function AgentToggle({ name, enabled, onChange, loading }: Props) {
  return (
    <div className={clsx(
      'flex items-center justify-between p-3 rounded border transition-colors',
      enabled ? 'border-accent-green/30 bg-accent-green/5' : 'border-border bg-bg-surface',
    )}>
      <div className="flex items-center gap-2.5">
        <span className={clsx(
          'w-2 h-2 rounded-full flex-shrink-0',
          enabled ? 'bg-accent-green animate-pulse-slow' : 'bg-text-muted',
        )} />
        <span className="text-xs text-text-primary font-medium">{name}</span>
      </div>

      {/* Toggle switch */}
      <button
        onClick={() => onChange(!enabled)}
        disabled={loading}
        aria-checked={enabled}
        role="switch"
        className={clsx(
          'relative inline-flex h-5 w-9 flex-shrink-0 rounded-full border-2 border-transparent',
          'transition-colors focus:outline-none disabled:opacity-50 disabled:cursor-not-allowed',
          enabled ? 'bg-accent-green' : 'bg-bg-overlay',
        )}
      >
        <span
          className={clsx(
            'pointer-events-none inline-block h-4 w-4 rounded-full bg-white shadow',
            'transform transition-transform',
            enabled ? 'translate-x-4' : 'translate-x-0',
          )}
        />
      </button>
    </div>
  )
}
