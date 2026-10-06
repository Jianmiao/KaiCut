import * as React from 'react'
import { createPortal } from 'react-dom'
import { Info, AlertTriangle, X } from 'lucide-react'
import { cn } from '@/lib/utils'
import { useDialogFocus } from '@/hooks/use-dialog-focus'
import { Button } from './button'

export interface DialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  title: string
  description?: string
  icon?: React.ReactNode
  confirmText?: string
  cancelText?: string
  destructive?: boolean
  onConfirm?: () => void | boolean | Promise<void | boolean>
  hideCancel?: boolean
  hideConfirm?: boolean
  confirmDisabled?: boolean
  children?: React.ReactNode
  className?: string
}

const Dialog: React.FC<DialogProps> = ({
  open, onOpenChange, title, description, icon, confirmText = '确定', cancelText = '取消',
  destructive = false, onConfirm, hideCancel = false, hideConfirm = false, confirmDisabled = false, children, className,
}) => {
  const panelRef = React.useRef<HTMLDivElement>(null)
  const id = React.useId()
  const [confirming, setConfirming] = React.useState(false)
  const [confirmError, setConfirmError] = React.useState<string | null>(null)
  const close = () => {
    if (!confirming) {
      setConfirmError(null)
      onOpenChange(false)
    }
  }
  useDialogFocus(open, panelRef, close)
  const confirm = async () => {
    if (confirmDisabled || confirming) return
    setConfirming(true)
    setConfirmError(null)
    try {
      const result = await onConfirm?.()
      if (result !== false) onOpenChange(false)
    } catch {
      setConfirmError('操作未完成，请检查输入或稍后重试。')
    } finally {
      setConfirming(false)
    }
  }
  if (!open || typeof document === 'undefined') return null

  return createPortal(
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/35 p-4"
      onClick={(event) => { if (event.target === event.currentTarget) close() }}>
      <div ref={panelRef} role="dialog" aria-modal="true" aria-labelledby={`${id}-title`}
        aria-describedby={description ? `${id}-description` : undefined} aria-busy={confirming || undefined} tabIndex={-1}
        className={cn('relative max-h-[calc(100dvh-32px)] w-full max-w-md overflow-y-auto rounded-[var(--kc-radius-dialog)] border border-[var(--kc-color-border)] bg-[var(--kc-color-content)] p-5 text-[var(--kc-color-text-primary)] shadow-2xl', className)}>
        <Button variant="ghost" size="icon" disabled={confirming} onClick={close}
          className="absolute right-2 top-2" aria-label="关闭对话框"><X className="h-4 w-4" aria-hidden="true" /></Button>
        <div className="flex items-start gap-3.5 pr-6">
          <div className={cn('flex h-10 w-10 shrink-0 items-center justify-center rounded-full bg-[var(--kc-color-content-muted)]', destructive ? 'text-[var(--kc-color-danger)]' : 'text-[var(--kc-color-text-secondary)]')} aria-hidden="true">
            {icon ?? (destructive ? <AlertTriangle className="h-5 w-5" /> : <Info className="h-5 w-5" />)}
          </div>
          <div className="min-w-0 flex-1 space-y-1">
            <h2 id={`${id}-title`} className="text-base font-semibold leading-tight">{title}</h2>
            {description && <p id={`${id}-description`} className="text-sm leading-relaxed text-[var(--kc-color-text-secondary)]">{description}</p>}
          </div>
        </div>
        {children && <div className="pt-3">{children}</div>}
        {confirmError && <p role="alert" className="mt-3 text-sm text-[var(--kc-color-danger)]">{confirmError}</p>}
        {(!hideCancel || !hideConfirm) && <div className="mt-5 flex items-center justify-end gap-2 border-t border-[var(--kc-color-divider)] pt-3.5">
          {!hideCancel && <Button variant="secondary" size="sm" disabled={confirming} onClick={close}>{cancelText}</Button>}
          {!hideConfirm && <Button variant={destructive ? 'destructive' : 'secondary'} size="sm" disabled={confirmDisabled || confirming} onClick={() => void confirm()}>{confirming ? '处理中…' : confirmText}</Button>}
        </div>}
      </div>
    </div>, document.body,
  )
}

export { Dialog }
