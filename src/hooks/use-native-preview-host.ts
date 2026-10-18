import { useEffect, useRef, type RefObject } from 'react'
import { invoke, isTauri } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { nativePreviewGeometry, type NativePreviewGeometry, type PreviewRect } from '@/lib/native-preview-host'

interface HostStatus { enabled: boolean; sessionId: string | null; revision: number }
interface HostOperation {
  command: 'native_preview_host_layout' | 'native_preview_host_release'
  args: Record<string, unknown>
  complete: (error?: unknown) => void
}

// Shared across StrictMode effects and preview remounts: one native mutation in
// flight, one replaceable pending operation, and monotonically ordered tickets.
let sequence = 0
let ownerSequence = 0
let currentOwner = 0
let running = false
let pending: HostOperation | null = null
function submit(operation: HostOperation) {
  pending = operation
  if (running) return
  const drain = async () => {
    running = true
    while (pending) {
      const next = pending
      pending = null
      try { await invoke(next.command, next.args); next.complete() }
      catch (error) { next.complete(error) }
    }
    running = false
  }
  void drain()
}

const rect = (element: Element): PreviewRect => {
  const value = element.getBoundingClientRect()
  return { x: value.x, y: value.y, width: value.width, height: value.height }
}
const cssPixels = (value: string) => value === '' ? 0 : Number.parseFloat(value)
const clipsOverflow = (value: string) => /^(hidden|clip|scroll|auto)$/.test(value)
const overlaySelector = '[role="dialog"], [role="alertdialog"], [role="alert"], [role="menu"], [role="listbox"], [role="tooltip"], .preview-playback-error, .preview-editing-notice, [data-native-preview-blocker]'

function rendered(element: Element) {
  for (let current: Element | null = element; current; current = current.parentElement) {
    const style = getComputedStyle(current)
    if (current.hasAttribute('hidden') || current.getAttribute('data-state') === 'closed' || style.display === 'none' || style.visibility === 'hidden' || style.visibility === 'collapse') return false
  }
  return true
}

function measure(canvas: HTMLElement): NativePreviewGeometry {
  const style = getComputedStyle(canvas)
  const viewport: PreviewRect = { x: 0, y: 0, width: window.innerWidth, height: window.innerHeight }
  const clips: PreviewRect[] = []
  const visual = window.visualViewport
  if (visual) clips.push({ x: visual.offsetLeft, y: visual.offsetTop, width: visual.width, height: visual.height })
  let transformed = false
  for (let ancestor: HTMLElement | null = canvas; ancestor; ancestor = ancestor.parentElement) {
    const computed = getComputedStyle(ancestor)
    // A rectangular native child cannot follow CSS rotations/perspective.
    if (computed.transform && computed.transform !== 'none') transformed = true
    if (ancestor === canvas) continue
    const x = clipsOverflow(computed.overflowX || computed.overflow)
    const y = clipsOverflow(computed.overflowY || computed.overflow)
    if (x || y) {
      const bounds = rect(ancestor)
      const left = cssPixels(computed.borderLeftWidth), right = cssPixels(computed.borderRightWidth)
      const top = cssPixels(computed.borderTopWidth), bottom = cssPixels(computed.borderBottomWidth)
      clips.push({ x: x ? bounds.x + left : 0, y: y ? bounds.y + top : 0,
        width: x ? bounds.width - left - right : viewport.width, height: y ? bounds.height - top - bottom : viewport.height })
    }
  }
  const controls = canvas.parentElement?.querySelector<HTMLElement>('.preview-controls')
  return nativePreviewGeometry({ canvas: rect(canvas),
    insets: { top: cssPixels(style.paddingTop) + cssPixels(style.borderTopWidth),
      right: cssPixels(style.paddingRight) + cssPixels(style.borderRightWidth),
      bottom: cssPixels(style.paddingBottom) + cssPixels(style.borderBottomWidth),
      left: cssPixels(style.paddingLeft) + cssPixels(style.borderLeftWidth) },
    viewport, clips, controls: controls && rendered(controls) ? rect(controls) : null, scaleFactor: window.devicePixelRatio,
    blocked: !canvas.isConnected || !rendered(canvas) || transformed || Array.from(document.querySelectorAll(overlaySelector)).some(rendered),
  })
}

export type NativeHostStatus = 'disabled' | 'pending' | 'attached' | 'blocked' | 'fallback'
/** Opt-in child host. An ordinary build only answers disabled status. */
export function useNativePreviewHost(canvasRef: RefObject<HTMLDivElement | null>, { enabled = true, onStatus }: {
  enabled?: boolean; onStatus?: (status: NativeHostStatus) => void
} = {}) {
  const callback = useRef(onStatus)
  useEffect(() => { callback.current = onStatus }, [onStatus])
  useEffect(() => {
    if (!enabled || !isTauri() || !canvasRef.current) return
    const canvas = canvasRef.current
    const owner = ++ownerSequence
    currentOwner = owner
    let alive = true
    let failed = false
    let sessionId: string | null = null
    let frame = 0
    let lastLayout = ''
    let lastTicket = 0
    const motionTargets = new Map<Element, Set<string>>()
    let disposeObservers = () => {}
    const active = () => alive && !failed && currentOwner === owner
    const report = (status: NativeHostStatus) => {
      canvas.dataset.nativePreviewHost = status
      callback.current?.(status)
    }
    const release = () => {
      if (!sessionId) return
      submit({ command: 'native_preview_host_release', args: { sessionId, sequence: ++sequence }, complete: () => {} })
    }
    const update = () => {
      frame = 0
      if (!active() || !sessionId) return
      const geometry = measure(canvas)
      for (const target of motionTargets.keys()) if (!target.isConnected) motionTargets.delete(target)
      if (motionTargets.size > 0) Object.assign(geometry, { x: 0, y: 0, width: 0, height: 0, blocked: true })
      const layout = { ...geometry, viewportWidth: window.innerWidth, viewportHeight: window.innerHeight }
      const signature = JSON.stringify(layout)
      if (signature === lastLayout) return
      lastLayout = signature
      report(geometry.blocked ? 'blocked' : 'pending')
      const ticket = ++sequence
      lastTicket = ticket
      submit({ command: 'native_preview_host_layout', args: { request: { sessionId, sequence: ticket, ...layout } },
        complete: (error) => {
          if (!active()) return
          if (error !== undefined) {
            failed = true
            report('fallback')
            disposeObservers()
            release()
          } else if (ticket === lastTicket) report(geometry.blocked ? 'blocked' : 'attached')
        },
      })
    }
    const schedule = () => { if (active() && !frame) frame = requestAnimationFrame(update) }
    const motion = (event: Event) => {
      const target = event.target
      if (!(target instanceof Element) || !target.contains(canvas)) { schedule(); return }
      const transition = event.type.startsWith('transition')
      const key = transition ? `transition:${(event as TransitionEvent).propertyName}` : `animation:${(event as AnimationEvent).animationName}`
      const keys = motionTargets.get(target) ?? new Set<string>()
      if (event.type === 'transitionrun' || event.type === 'animationstart') keys.add(key)
      else keys.delete(key)
      if (keys.size > 0) motionTargets.set(target, keys)
      else motionTargets.delete(target)
      schedule()
    }
    void Promise.resolve().then(() => invoke<HostStatus>('native_preview_host_status')).then((status) => {
      if (!active()) return
      if (!status?.enabled || !status.sessionId) { report('disabled'); return }
      if (Number.isSafeInteger(status.revision) && status.revision >= 0) sequence = Math.max(sequence, status.revision)
      sessionId = status.sessionId
      const resize = new ResizeObserver(schedule)
      let unlisten: (() => void) | null = null
      for (let element: Element | null = canvas; element; element = element.parentElement) resize.observe(element)
      const controls = canvas.parentElement?.querySelector('.preview-controls')
      if (controls) resize.observe(controls)
      const mutation = new MutationObserver(schedule)
      mutation.observe(document.body, { subtree: true, childList: true, attributes: true,
        attributeFilter: ['class', 'style', 'hidden', 'aria-hidden', 'open', 'role', 'data-state', 'data-native-preview-blocker'] })
      window.addEventListener('resize', schedule)
      document.addEventListener('scroll', schedule, true)
      for (const name of ['transitionrun', 'transitionend', 'transitioncancel', 'animationstart', 'animationend', 'animationcancel']) document.addEventListener(name, motion, true)
      window.visualViewport?.addEventListener('resize', schedule)
      window.visualViewport?.addEventListener('scroll', schedule)
      disposeObservers = () => {
        unlisten?.()
        unlisten = null
        resize.disconnect(); mutation.disconnect()
        window.removeEventListener('resize', schedule)
        document.removeEventListener('scroll', schedule, true)
        for (const name of ['transitionrun', 'transitionend', 'transitioncancel', 'animationstart', 'animationend', 'animationcancel']) document.removeEventListener(name, motion, true)
        window.visualViewport?.removeEventListener('resize', schedule)
        window.visualViewport?.removeEventListener('scroll', schedule)
        if (frame) cancelAnimationFrame(frame)
        frame = 0
      }
      void listen('kaicut:native-preview-invalidated', () => { lastLayout = ''; schedule() }, { target: { kind: 'Window', label: 'main' } })
        .then((stop) => {
          if (!active()) { stop(); return }
          unlisten = stop
          schedule()
        }).catch(() => {
          if (!active()) return
          failed = true
          report('fallback')
          disposeObservers()
          release()
        })
    }).catch(() => { if (active()) report('fallback') })
    return () => {
      alive = false
      disposeObservers()
      // A superseded effect must never enqueue a release behind its replacement.
      if (currentOwner === owner) release()
    }
  }, [canvasRef, enabled])
}
