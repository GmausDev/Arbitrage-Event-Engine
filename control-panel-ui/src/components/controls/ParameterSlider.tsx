'use client'

interface Props {
  label:    string
  value:    number
  min:      number
  max:      number
  step:     number
  format?:  (v: number) => string
  onChange: (v: number) => void
  disabled?: boolean
}

export function ParameterSlider({ label, value, min, max, step, format, onChange, disabled }: Props) {
  const fmt = format ?? ((v: number) => v.toFixed(2))
  const pct = ((value - min) / (max - min)) * 100

  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between text-xs">
        <span className="text-text-secondary">{label}</span>
        <span className="text-text-primary font-semibold font-mono w-16 text-right">{fmt(value)}</span>
      </div>
      <div className="relative">
        <input
          type="range"
          min={min}
          max={max}
          step={step}
          value={value}
          disabled={disabled}
          onChange={e => onChange(Number(e.target.value))}
          className="
            w-full h-1.5 rounded-full appearance-none cursor-pointer
            bg-bg-overlay disabled:opacity-40 disabled:cursor-not-allowed
            [&::-webkit-slider-thumb]:appearance-none
            [&::-webkit-slider-thumb]:w-3.5
            [&::-webkit-slider-thumb]:h-3.5
            [&::-webkit-slider-thumb]:rounded-full
            [&::-webkit-slider-thumb]:bg-accent-blue
            [&::-webkit-slider-thumb]:cursor-pointer
            [&::-webkit-slider-thumb]:border-2
            [&::-webkit-slider-thumb]:border-bg-base
          "
          style={{
            background: `linear-gradient(to right, #3b82f6 ${pct}%, #1e2330 ${pct}%)`,
          }}
        />
      </div>
      <div className="flex justify-between text-xs text-text-muted">
        <span>{fmt(min)}</span>
        <span>{fmt(max)}</span>
      </div>
    </div>
  )
}
