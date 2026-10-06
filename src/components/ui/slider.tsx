import * as React from 'react'
import { cn } from '@/lib/utils'

export interface SliderProps {
  value?: number
  defaultValue?: number
  min?: number
  max?: number
  step?: number
  onChange?: (value: number) => void
  className?: string
  disabled?: boolean
  id?: string
  label?: string
  'aria-label'?: string
  'aria-labelledby'?: string
  'aria-valuetext'?: string
}

const Slider: React.FC<SliderProps> = ({ value: controlledValue, defaultValue = 50, min = 0, max = 100, step = 1, onChange, className, disabled = false, id, label, 'aria-label': ariaLabel, 'aria-labelledby': ariaLabelledBy, 'aria-valuetext': ariaValueText }) => {
  const [uncontrolledValue, setUncontrolledValue] = React.useState(defaultValue)
  const generatedId = React.useId()
  const inputId = id ?? generatedId
  const rawValue = controlledValue !== undefined ? controlledValue : uncontrolledValue
  const safeMax = Math.max(min, max)
  const value = Math.min(safeMax, Math.max(min, Number.isFinite(rawValue) ? rawValue : min))
  const percentage = safeMax > min ? ((value - min) / (safeMax - min)) * 100 : 0
  const inactive = disabled || safeMax <= min
  return <div className={className}>
    {label && <label htmlFor={inputId} className="mb-1 block text-sm text-[var(--kc-color-text-secondary)]">{label}</label>}
    <div className={cn('relative flex min-h-8 w-full touch-none select-none items-center rounded-md py-1.5 has-[:focus-visible]:outline-2 has-[:focus-visible]:outline-offset-2 has-[:focus-visible]:outline-[var(--kc-color-focus)]', inactive && 'opacity-50')}>
      <div className="relative h-1.5 w-full grow overflow-hidden rounded-full bg-[var(--kc-color-border)]" aria-hidden="true">
        <div className="h-full bg-[var(--kc-color-accent)]" style={{ width: `${percentage}%` }} />
      </div>
      <input id={inputId} aria-label={ariaLabel ?? (!label && !ariaLabelledBy && !id ? '数值' : undefined)} aria-labelledby={ariaLabelledBy} aria-valuetext={ariaValueText}
        type="range" min={min} max={safeMax} step={step > 0 ? step : 1} value={value} disabled={inactive}
        onChange={(event) => {
          const next = Number(event.target.value)
          if (controlledValue === undefined) setUncontrolledValue(next)
          onChange?.(next)
        }}
        className="absolute inset-0 z-10 h-full w-full cursor-pointer opacity-0 disabled:cursor-not-allowed" />
      <div className="pointer-events-none absolute h-4 w-4 -translate-x-1/2 rounded-full border border-[var(--kc-color-border)] bg-[var(--kc-color-content)] shadow-md" style={{ left: `${percentage}%` }} aria-hidden="true" />
    </div>
  </div>
}

export { Slider }
