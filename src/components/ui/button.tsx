import * as React from 'react'
import { cn } from '@/lib/utils'

export interface ButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: 'default' | 'secondary' | 'destructive' | 'outline' | 'ghost' | 'link' | 'accent'
  size?: 'default' | 'sm' | 'lg' | 'icon' | 'xs'
}

const Button = React.forwardRef<HTMLButtonElement, ButtonProps>(
  ({ className, variant = 'default', size = 'default', type = 'button', ...props }, ref) => <button
    ref={ref} type={type} {...props}
    className={cn('inline-flex cursor-default select-none items-center justify-center gap-2 whitespace-nowrap rounded-[var(--kc-radius-control)] text-sm font-medium transition-colors duration-[var(--kc-duration-press)] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--kc-color-focus)] disabled:cursor-not-allowed disabled:opacity-50', {
      'border border-transparent bg-[var(--kc-color-accent)] text-[var(--kc-color-text-on-accent)] enabled:hover:bg-[var(--kc-color-accent-hover)] enabled:active:bg-[var(--kc-color-accent-pressed,var(--kc-color-accent-hover))]': variant === 'default' || variant === 'accent',
      'border border-[var(--kc-color-border)] bg-[var(--kc-color-content-muted)] text-[var(--kc-color-text-primary)] enabled:hover:bg-[var(--kc-color-selection)]': variant === 'secondary',
      'border border-transparent bg-[var(--kc-color-danger)] text-white enabled:hover:brightness-90': variant === 'destructive',
      'border border-[var(--kc-color-border)] bg-transparent text-[var(--kc-color-text-primary)] enabled:hover:bg-[var(--kc-color-content-muted)]': variant === 'outline',
      'border border-transparent bg-transparent text-[var(--kc-color-text-primary)] enabled:hover:bg-[var(--kc-color-content-muted)]': variant === 'ghost',
      'border-0 bg-transparent text-[var(--kc-color-accent)] enabled:hover:underline': variant === 'link',
      'h-8 px-3 py-1': size === 'default',
      'h-8 px-2.5': size === 'sm',
      'h-6 min-w-6 px-1.5 text-xs': size === 'xs',
      'h-10 px-4': size === 'lg',
      'h-8 w-8 p-0': size === 'icon',
    }, className)} />,
)
Button.displayName = 'Button'

export { Button }
