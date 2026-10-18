import * as React from 'react'
import { cn } from '@/lib/utils'

export interface MenuItem {
  id: string
  label: string
  icon?: React.ReactNode
  shortcut?: string
  destructive?: boolean
  disabled?: boolean
  onClick?: () => void
}

export interface DropdownMenuProps {
  trigger: React.ReactNode
  items: (MenuItem | 'separator')[]
  align?: 'left' | 'right'
  className?: string
}

type TriggerProps = React.ButtonHTMLAttributes<HTMLButtonElement> & { 'data-menu-trigger'?: boolean }

const DropdownMenu: React.FC<DropdownMenuProps> = ({ trigger, items, align = 'left', className }) => {
  const [isOpen, setIsOpen] = React.useState(false)
  const containerRef = React.useRef<HTMLDivElement>(null)
  const initialFocus = React.useRef<'first' | 'last'>('first')
  const id = React.useId()
  const focusTrigger = () => containerRef.current?.querySelector<HTMLElement>('[data-menu-trigger]')?.focus()
  const close = () => { focusTrigger(); setIsOpen(false) }
  const enabledItems = () => Array.from(containerRef.current?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not(:disabled)') ?? [])

  React.useLayoutEffect(() => {
    if (!isOpen) return
    const enabled = Array.from(containerRef.current?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not(:disabled)') ?? [])
    const target = initialFocus.current === 'last' ? enabled.at(-1) : enabled[0]
    target?.focus()
  }, [isOpen])

  React.useEffect(() => {
    if (!isOpen) return
    const handlePointer = (event: PointerEvent) => {
      if (containerRef.current && !containerRef.current.contains(event.target as Node)) setIsOpen(false)
    }
    document.addEventListener('pointerdown', handlePointer)
    return () => document.removeEventListener('pointerdown', handlePointer)
  }, [isOpen])

  const triggerElement = React.isValidElement<TriggerProps>(trigger) ? trigger : <button type="button">{trigger}</button>
  const triggerProps: TriggerProps = {
    id: triggerElement.props.id ?? `${id}-trigger`,
    'data-menu-trigger': true,
    'aria-haspopup': 'menu',
    'aria-expanded': isOpen,
    'aria-controls': isOpen ? `${id}-menu` : undefined,
    'aria-label': triggerElement.props['aria-label'] ?? '打开菜单',

  }

  return <div ref={containerRef} className="relative inline-block text-left"
    onClick={(event) => {
      if (!(event.target as Element).closest('[data-menu-trigger]') || event.defaultPrevented) return
      initialFocus.current = 'first'
      setIsOpen(!isOpen)
    }}
    onKeyDown={(event) => {
      if (!(event.target as Element).closest('[data-menu-trigger]') || event.defaultPrevented || event.nativeEvent.isComposing) return
      if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
        event.preventDefault()
        initialFocus.current = event.key === 'ArrowUp' ? 'last' : 'first'
        if (isOpen) {
          const enabled = enabledItems()
          const target = initialFocus.current === 'last' ? enabled.at(-1) : enabled[0]
          target?.focus()
        } else setIsOpen(true)
      } else if (event.key === 'Escape' && isOpen) { event.stopPropagation(); close() }
    }}
    onBlur={(event) => {
    if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setIsOpen(false)
  }}>
    {React.cloneElement(triggerElement, triggerProps)}
    {isOpen && <div id={`${id}-menu`} role="menu" aria-labelledby={triggerProps.id}
      className={cn('absolute z-50 mt-1.5 min-w-[200px] select-none rounded-[var(--kc-radius-popover)] border border-[var(--kc-color-border)] p-1.5 macos-popover', align === 'right' ? 'right-0' : 'left-0', className)}
      onKeyDown={(event) => {
        if (event.nativeEvent.isComposing) return
        const enabled = enabledItems()
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
          search.find((item) => item.textContent?.trim().toLocaleLowerCase().startsWith(event.key.toLocaleLowerCase()))?.focus()
        }
        if (next !== undefined) { event.preventDefault(); enabled[next]?.focus() }
      }}>
      {items.map((item, index) => item === 'separator'
        ? <div key={`separator-${index}`} role="separator" className="my-1.5 h-px bg-[var(--kc-color-divider)]" />
        : <button key={item.id} type="button" role="menuitem" aria-label={item.label} disabled={item.disabled} tabIndex={-1}
          onClick={() => { close(); item.onClick?.() }}
          className={cn('group flex min-h-8 w-full cursor-default items-center justify-between rounded-[var(--kc-radius-control)] px-2.5 text-sm transition-colors duration-[var(--kc-duration-press)] focus-visible:outline-2 focus-visible:outline-[var(--kc-color-focus)] disabled:cursor-not-allowed disabled:opacity-50',
            item.destructive ? 'text-[var(--kc-color-danger)] enabled:hover:bg-[var(--kc-color-danger)] enabled:hover:text-white' : 'text-[var(--kc-color-text-primary)] enabled:hover:bg-[var(--kc-color-selection)]')}>
          <span className="flex min-w-0 items-center gap-2.5">{item.icon && <span className="flex h-4 w-4 shrink-0 items-center justify-center [&>svg]:h-3.5 [&>svg]:w-3.5" aria-hidden="true">{item.icon}</span>}<span className="truncate font-medium">{item.label}</span></span>
          {item.shortcut && <span className="ml-4 font-mono text-xs text-[var(--kc-color-text-secondary)]" aria-hidden="true">{item.shortcut}</span>}
        </button>)}
    </div>}
  </div>
}

export { DropdownMenu }

