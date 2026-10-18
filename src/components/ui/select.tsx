import * as React from 'react'
import { ChevronDown, Check } from 'lucide-react'
import { cn } from '@/lib/utils'

export interface SelectOption { value: string; label: string; disabled?: boolean }
export interface SelectProps {
  options: SelectOption[]
  value?: string
  defaultValue?: string
  onChange?: (value: string) => void
  placeholder?: string
  className?: string
  disabled?: boolean
  id?: string
  'aria-label'?: string
  'aria-labelledby'?: string
}

const Select: React.FC<SelectProps> = ({ options, value: controlledValue, defaultValue = '', onChange, placeholder = '请选择…', className, disabled = false, id, 'aria-label': ariaLabel, 'aria-labelledby': ariaLabelledBy }) => {
  const [isOpen, setIsOpen] = React.useState(false)
  const [uncontrolledValue, setUncontrolledValue] = React.useState(defaultValue)
  const containerRef = React.useRef<HTMLDivElement>(null)
  const triggerRef = React.useRef<HTMLButtonElement>(null)
  const initialFocus = React.useRef<'selected' | 'first' | 'last'>('selected')
  const generatedId = React.useId()
  const triggerId = id ?? `${generatedId}-trigger`
  const listId = `${generatedId}-options`
  const selectedValue = controlledValue !== undefined ? controlledValue : uncontrolledValue
  const selectedOption = options.find((option) => option.value === selectedValue)
  const expanded = isOpen && !disabled
  const close = () => { triggerRef.current?.focus(); setIsOpen(false) }

  React.useLayoutEffect(() => {
    if (!expanded) return
    const enabled = Array.from(containerRef.current?.querySelectorAll<HTMLButtonElement>('[role="option"]:not(:disabled)') ?? [])
    const selected = enabled.find((option) => option.getAttribute('aria-selected') === 'true')
    const target = initialFocus.current === 'last' ? enabled.at(-1) : initialFocus.current === 'first' ? enabled[0] : selected ?? enabled[0]
    target?.focus()
  }, [expanded])

  React.useEffect(() => {
    if (!expanded) return
    const handlePointer = (event: PointerEvent) => {
      if (containerRef.current && !containerRef.current.contains(event.target as Node)) setIsOpen(false)
    }
    document.addEventListener('pointerdown', handlePointer)
    return () => document.removeEventListener('pointerdown', handlePointer)
  }, [expanded])

  const handleSelect = (value: string) => {
    if (controlledValue === undefined) setUncontrolledValue(value)
    close()
    onChange?.(value)
  }

  return <div ref={containerRef} className="relative inline-block w-full text-left" onBlur={(event) => {
    if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setIsOpen(false)
  }}>
    <button ref={triggerRef} id={triggerId} aria-label={ariaLabel ?? (!id && !ariaLabelledBy ? placeholder : undefined)} aria-labelledby={ariaLabelledBy}
      aria-haspopup="listbox" aria-expanded={expanded} aria-controls={expanded ? listId : undefined}
      type="button" disabled={disabled}
      onClick={() => { initialFocus.current = 'selected'; setIsOpen(!expanded) }}
      onKeyDown={(event) => {
        if (event.nativeEvent.isComposing) return
        if (event.key === 'ArrowDown' || event.key === 'ArrowUp' || event.key === 'Home' || event.key === 'End') {
          event.preventDefault()
          initialFocus.current = event.key === 'End' || event.key === 'ArrowUp' ? 'last' : event.key === 'Home' ? 'first' : 'selected'
          setIsOpen(true)
        } else if (event.key === 'Escape' && expanded) { event.stopPropagation(); close() }
      }}
      className={cn('flex h-8 w-full cursor-default select-none items-center justify-between rounded-[var(--kc-radius-control)] border border-[var(--kc-color-border)] bg-[var(--kc-color-content)] px-2.5 py-1 text-sm text-[var(--kc-color-text-primary)] transition-colors duration-[var(--kc-duration-press)] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--kc-color-focus)] disabled:cursor-not-allowed disabled:opacity-50', className)}>
      <span className="truncate font-medium">{selectedOption?.label ?? placeholder}</span>
      <ChevronDown className="ml-1.5 h-3.5 w-3.5 shrink-0 text-[var(--kc-color-text-secondary)]" aria-hidden="true" />
    </button>
    {expanded && <div id={listId} role="listbox" aria-labelledby={ariaLabelledBy ?? triggerId}
      className="absolute left-0 top-full z-50 mt-1.5 max-h-64 w-full min-w-[160px] select-none overflow-y-auto rounded-[var(--kc-radius-popover)] border border-[var(--kc-color-border)] p-1.5 macos-popover"
      onKeyDown={(event) => {
        if (event.nativeEvent.isComposing) return
        const enabled = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="option"]:not(:disabled)'))
        const index = enabled.indexOf(document.activeElement as HTMLButtonElement)
        let next: number | undefined
        if (event.key === 'ArrowDown') next = (index + 1) % enabled.length
        else if (event.key === 'ArrowUp') next = (index - 1 + enabled.length) % enabled.length
        else if (event.key === 'Home') next = 0
        else if (event.key === 'End') next = enabled.length - 1
        else if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); close() }
        else if (event.key === 'Tab') close()
        else if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
          const search = [...enabled.slice(index + 1), ...enabled.slice(0, index + 1)]
          search.find((option) => option.textContent?.trim().toLocaleLowerCase().startsWith(event.key.toLocaleLowerCase()))?.focus()
        }
        if (next !== undefined) { event.preventDefault(); enabled[next]?.focus() }
      }}>
      {options.map((option) => <button key={option.value} type="button" role="option" aria-selected={option.value === selectedValue} aria-label={option.label}
        disabled={option.disabled} tabIndex={-1} onClick={() => handleSelect(option.value)}
        className={cn('flex min-h-8 w-full cursor-default select-none items-center justify-between rounded-[var(--kc-radius-control)] px-2.5 text-sm transition-colors duration-[var(--kc-duration-press)] focus-visible:outline-2 focus-visible:outline-[var(--kc-color-focus)] disabled:cursor-not-allowed disabled:opacity-50',
          option.value === selectedValue ? 'bg-[var(--kc-color-selection)] font-medium text-[var(--kc-color-text-primary)]' : 'text-[var(--kc-color-text-primary)] enabled:hover:bg-[var(--kc-color-content-muted)]')}>
        <span className="truncate">{option.label}</span>
        {option.value === selectedValue && <Check className="ml-2 h-3.5 w-3.5 shrink-0" aria-hidden="true" />}
      </button>)}
    </div>}
  </div>
}

export { Select }
