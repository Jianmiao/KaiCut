import * as React from 'react'

const modalStack: HTMLElement[] = []
let lockedRoot: HTMLElement | null = null
let rootWasInert = false
let previousOverflow = ''

const focusableSelector = [
  'button:not([disabled])', 'a[href]', 'input:not([disabled]):not([type="hidden"])',
  'select:not([disabled])', 'textarea:not([disabled])', '[tabindex]:not([tabindex="-1"])',
].join(',')

function focusableElements(panel: HTMLElement) {
  return Array.from(panel.querySelectorAll<HTMLElement>(focusableSelector)).filter(
    (element) => !element.closest('[hidden], [inert], [aria-hidden="true"]') &&
      getComputedStyle(element).display !== 'none' && getComputedStyle(element).visibility !== 'hidden',
  )
}

/** Keeps modal focus inside its portal while the application underneath is inert. */
export function useDialogFocus(open: boolean, panelRef: React.RefObject<HTMLDivElement | null>, onClose: () => void) {
  const onCloseRef = React.useRef(onClose)
  React.useLayoutEffect(() => { onCloseRef.current = onClose }, [onClose])

  React.useLayoutEffect(() => {
    const panel = panelRef.current
    if (!open || !panel) return
    const returnTarget = document.activeElement instanceof HTMLElement ? document.activeElement : null
    if (modalStack.length === 0) {
      lockedRoot = document.getElementById('root')
      rootWasInert = lockedRoot?.hasAttribute('inert') ?? false
      lockedRoot?.setAttribute('inert', '')
      previousOverflow = document.body.style.overflow
      document.body.style.overflow = 'hidden'
    }
    modalStack.push(panel)
    const focusFirst = () => {
      const first = panel.querySelector<HTMLElement>('[data-dialog-autofocus]') ?? focusableElements(panel)[0] ?? panel
      first.focus({ preventScroll: true })
    }
    focusFirst()
    const handleKeyDown = (event: KeyboardEvent) => {
      if (modalStack.at(-1) !== panel || event.isComposing) return
      if (event.key === 'Escape') {
        event.preventDefault()
        event.stopPropagation()
        onCloseRef.current()
      } else if (event.key === 'Tab') {
        const focusable = focusableElements(panel)
        const first = focusable[0]
        const last = focusable.at(-1)
        if (!first || !last) {
          event.preventDefault()
          panel.focus()
        } else if (event.shiftKey && (document.activeElement === first || document.activeElement === panel || !panel.contains(document.activeElement))) {
          event.preventDefault()
          last.focus()
        } else if (!event.shiftKey && (document.activeElement === last || document.activeElement === panel || !panel.contains(document.activeElement))) {
          event.preventDefault()
          first.focus()
        }
      }
    }
    const handleFocusIn = (event: FocusEvent) => {
      if (modalStack.at(-1) === panel && !panel.contains(event.target as Node)) focusFirst()
    }
    document.addEventListener('keydown', handleKeyDown)
    document.addEventListener('focusin', handleFocusIn)
    return () => {
      document.removeEventListener('keydown', handleKeyDown)
      document.removeEventListener('focusin', handleFocusIn)
      const index = modalStack.indexOf(panel)
      if (index !== -1) modalStack.splice(index, 1)
      if (modalStack.length === 0) {
        if (!rootWasInert) lockedRoot?.removeAttribute('inert')
        document.body.style.overflow = previousOverflow
        lockedRoot = null
      }
      if (returnTarget?.isConnected && !returnTarget.closest('[inert]')) returnTarget.focus({ preventScroll: true })
      else modalStack.at(-1)?.focus({ preventScroll: true })
    }
  }, [open, panelRef])
}


