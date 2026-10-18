import * as React from 'react'
import { cn } from '@/lib/utils'

interface TabsContextValue { value: string; onValueChange: (value: string) => void; id: string; label?: string }
const TabsContext = React.createContext<TabsContextValue | undefined>(undefined)

export interface TabsProps extends React.HTMLAttributes<HTMLDivElement> {
  defaultValue?: string
  value?: string
  onValueChange?: (value: string) => void
}

const Tabs = React.forwardRef<HTMLDivElement, TabsProps>(
  ({ defaultValue, value: controlledValue, onValueChange, className, children, ...props }, ref) => {
    const [uncontrolledValue, setUncontrolledValue] = React.useState(defaultValue ?? '')
    const id = React.useId()
    const value = controlledValue !== undefined ? controlledValue : uncontrolledValue
    const handleValueChange = (next: string) => {
      if (controlledValue === undefined) setUncontrolledValue(next)
      onValueChange?.(next)
    }
    return <TabsContext.Provider value={{ value, onValueChange: handleValueChange, id, label: props['aria-label'] }}>
      <div ref={ref} className={cn('w-full', className)} {...props}>{children}</div>
    </TabsContext.Provider>
  },
)
Tabs.displayName = 'Tabs'

const TabsList = React.forwardRef<HTMLDivElement, React.HTMLAttributes<HTMLDivElement>>(
  ({ className, onKeyDown, ...props }, ref) => {
    const context = React.useContext(TabsContext)
    return <div ref={ref} {...props} role="tablist" aria-label={props['aria-label'] ?? context?.label}
      className={cn('inline-flex min-h-8 select-none items-center justify-center rounded-[var(--kc-radius-control)] border border-[var(--kc-color-border)] bg-[var(--kc-color-content-muted)] p-0.5 text-[var(--kc-color-text-secondary)]', className)}
      onKeyDown={(event) => {
        onKeyDown?.(event)
        if (event.defaultPrevented || event.nativeEvent.isComposing) return
        const tabs = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="tab"]:not(:disabled)'))
        const index = tabs.indexOf(document.activeElement as HTMLButtonElement)
        const vertical = props['aria-orientation'] === 'vertical'
        const rtl = getComputedStyle(event.currentTarget).direction === 'rtl'
        const nextKey = vertical ? 'ArrowDown' : rtl ? 'ArrowLeft' : 'ArrowRight'
        const previousKey = vertical ? 'ArrowUp' : rtl ? 'ArrowRight' : 'ArrowLeft'
        let next: number | undefined
        if (event.key === nextKey) next = (index + 1) % tabs.length
        else if (event.key === previousKey) next = (index - 1 + tabs.length) % tabs.length
        else if (event.key === 'Home') next = 0
        else if (event.key === 'End') next = tabs.length - 1
        if (next !== undefined) {
          event.preventDefault()
          tabs[next]?.focus()
          tabs[next]?.click()
        }
      }} />
  },
)
TabsList.displayName = 'TabsList'

export interface TabsTriggerProps extends React.ButtonHTMLAttributes<HTMLButtonElement> { value: string }

const TabsTrigger = React.forwardRef<HTMLButtonElement, TabsTriggerProps>(
  ({ className, value, children, onClick, disabled, ...props }, ref) => {
    const context = React.useContext(TabsContext)
    const selected = context?.value === value
    const part = encodeURIComponent(value)
    return <button ref={ref} {...props} id={`${context?.id}-tab-${part}`} type="button" role="tab"
      aria-selected={selected} aria-controls={`${context?.id}-panel-${part}`}
      aria-label={props['aria-label'] ?? (typeof children === 'string' ? children : value)}
      disabled={disabled} tabIndex={selected && !disabled ? 0 : -1}
      onClick={(event) => { onClick?.(event); if (!event.defaultPrevented) context?.onValueChange(value) }}
      className={cn('inline-flex min-h-7 cursor-default items-center justify-center whitespace-nowrap rounded-[var(--kc-radius-control)] px-2.5 py-0.5 text-sm font-medium transition-colors duration-[var(--kc-duration-press)] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--kc-color-focus)] disabled:cursor-not-allowed disabled:opacity-50',
        selected ? 'border border-[var(--kc-color-border)] bg-[var(--kc-color-content)] text-[var(--kc-color-text-primary)] shadow-xs' : 'border border-transparent text-[var(--kc-color-text-secondary)] enabled:hover:bg-[var(--kc-color-content)] enabled:hover:text-[var(--kc-color-text-primary)]', className)}>
      {children}
    </button>
  },
)
TabsTrigger.displayName = 'TabsTrigger'

export interface TabsContentProps extends React.HTMLAttributes<HTMLDivElement> { value: string }
const TabsContent = React.forwardRef<HTMLDivElement, TabsContentProps>(
  ({ className, value, children, ...props }, ref) => {
    const context = React.useContext(TabsContext)
    const selected = context?.value === value
    const part = encodeURIComponent(value)
    return <div ref={ref} {...props} id={`${context?.id}-panel-${part}`} role="tabpanel"
      aria-labelledby={`${context?.id}-tab-${part}`} hidden={!selected} tabIndex={0}
      className={cn('mt-2 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--kc-color-focus)]', className)}>
      {selected ? children : null}
    </div>
  },
)
TabsContent.displayName = 'TabsContent'

export { Tabs, TabsList, TabsTrigger, TabsContent }
