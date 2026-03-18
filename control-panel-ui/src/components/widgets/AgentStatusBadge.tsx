import clsx from 'clsx'

interface Props {
  name:    string
  enabled: boolean
  onClick?: () => void
}

export function AgentStatusBadge({ name, enabled, onClick }: Props) {
  return (
    <button
      onClick={onClick}
      className={clsx(
        'flex items-center gap-1.5 px-2.5 py-1 rounded border text-xs transition-colors',
        enabled
          ? 'border-accent-green/40 bg-accent-green/10 text-accent-green hover:bg-accent-green/20'
          : 'border-border text-text-muted hover:bg-bg-raised',
      )}
    >
      <span className={clsx(
        'w-1.5 h-1.5 rounded-full',
        enabled ? 'bg-accent-green animate-pulse-slow' : 'bg-text-muted',
      )} />
      {name}
    </button>
  )
}
