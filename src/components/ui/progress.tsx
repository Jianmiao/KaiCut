import * as React from 'react'
import { cn } from '@/lib/utils'

export interface ProgressProps extends React.HTMLAttributes<HTMLDivElement> {
  value?: number
  max?: number
  indeterminate?: boolean
}

const Progress = React.forwardRef<HTMLDivElement, ProgressProps>(
  ({ className, value = 0, max = 100, indeterminate = false, ...props }, ref) => {
    const safeMax = Number.isFinite(max) && max > 0 ? max : 100
    const safeValue = Number.isFinite(value) ? Math.max(0, Math.min(safeMax, value)) : 0
    const percentage = (safeValue / safeMax) * 100
    return <div ref={ref} {...props} role="progressbar"
      aria-label={props['aria-label'] ?? (props['aria-labelledby'] ? undefined : '进度')}
      aria-valuemin={0} aria-valuemax={safeMax} aria-valuenow={indeterminate ? undefined : safeValue}
      aria-valuetext={props['aria-valuetext'] ?? (indeterminate ? '进行中，进度未知' : undefined)}
      className={cn('relative h-2 w-full overflow-hidden rounded-full bg-[var(--kc-color-border)]', className)}>
      <div className={cn('h-full rounded-full bg-[var(--kc-color-accent)]', indeterminate && 'mx-auto opacity-50')}
        style={{ width: indeterminate ? '2rem' : `${percentage}%` }} aria-hidden="true" />
    </div>
  },
)
Progress.displayName = 'Progress'

export { Progress }

