import * as React from 'react'
import { createPortal } from 'react-dom'
import { X } from 'lucide-react'
import { cn } from '@/lib/utils'
import { useDialogFocus } from '@/hooks/use-dialog-focus'
import { Button } from './button'

export interface SheetProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  title?: string
  description?: string
  children: React.ReactNode
  side?: 'right' | 'left'
  className?: string
}

const Sheet: React.FC<SheetProps> = ({ open, onOpenChange, title, description, children, side = 'right', className }) => {
  const panelRef = React.useRef<HTMLDivElement>(null)
  const id = React.useId()
  useDialogFocus(open, panelRef, () => onOpenChange(false))
  if (!open || typeof document === 'undefined') return null
  return createPortal(
    <div className={cn('kc-sheet-backdrop fixed inset-0 z-50 flex bg-black/35', side === 'right' ? 'justify-end' : 'justify-start')}
      onClick={(event) => { if (event.target === event.currentTarget) onOpenChange(false) }}>
      <div ref={panelRef} role="dialog" aria-modal="true" tabIndex={-1}
        aria-labelledby={title ? `${id}-title` : undefined} aria-label={title ? undefined : '侧边面板'}
        aria-describedby={description ? `${id}-description` : undefined}
        className={cn('flex h-full w-full max-w-sm flex-col overflow-y-auto border-[var(--kc-color-border)] bg-[var(--kc-color-content)] p-5 text-[var(--kc-color-text-primary)] shadow-[var(--kc-shadow-dialog)]', side === 'right' ? 'kc-sheet-panel-right border-l' : 'kc-sheet-panel-left border-r', className)}>
        <div className="mb-4 flex items-start justify-between gap-3 border-b border-[var(--kc-color-divider)] pb-3">
          <div>
            {title && <h2 id={`${id}-title`} className="text-sm font-semibold">{title}</h2>}
            {description && <p id={`${id}-description`} className="mt-1 text-sm text-[var(--kc-color-text-secondary)]">{description}</p>}
          </div>
          <Button variant="ghost" size="icon" onClick={() => onOpenChange(false)} aria-label="关闭侧边面板"><X className="h-4 w-4" aria-hidden="true" /></Button>
        </div>
        <div className="space-y-3">{children}</div>
      </div>
    </div>, document.body,
  )
}

export { Sheet }
