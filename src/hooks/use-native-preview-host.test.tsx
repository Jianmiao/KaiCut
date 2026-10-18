import { act, StrictMode, useRef } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { useNativePreviewHost } from './use-native-preview-host'

vi.mock('@tauri-apps/api/core', () => ({ isTauri: () => true, invoke: vi.fn() }))
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }))
let host: HTMLDivElement
let root: Root
let frames: Map<number, FrameRequestCallback>
let frameId: number
let resizeCallbacks: Array<() => void>
let canvasX: number
let invalidate: (() => void) | undefined
let unlisten = vi.fn<() => void>()
const status = { enabled: true, sessionId: 'host-session', revision: 100 }
const mockInvoke = vi.mocked(invoke)
const calls = (name: string) => mockInvoke.mock.calls.filter(([command]) => command === name)
  .map(([command, args]) => [command, args as Record<string, unknown> | undefined] as const)
function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (value: Error) => void
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}
function Fixture({ enabled = true, onStatus }: { enabled?: boolean; onStatus?: (status: string) => void }) {
  const canvas = useRef<HTMLDivElement>(null)
  useNativePreviewHost(canvas, { enabled, onStatus })
  return <div><div ref={canvas} className="preview-canvas" style={{ paddingBottom: 52 }} /><div className="preview-controls" /></div>
}
async function tick() {
  await act(async () => {
    const work = [...frames.values()]; frames.clear()
    work.forEach((callback) => callback(performance.now()))
  })
}
async function mount(node = <Fixture />) { await act(async () => root.render(node)); await tick() }
async function resize(x: number) { canvasX = x; await act(async () => resizeCallbacks.forEach((callback) => callback())); await tick() }

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true })
  frameId = 0; frames = new Map(); resizeCallbacks = []; canvasX = 100
  mockInvoke.mockReset().mockImplementation(async (command) => command === 'native_preview_host_status' ? status : { ...status, revision: 101 })
  invalidate = undefined
  unlisten = vi.fn<() => void>()
  vi.mocked(listen).mockReset().mockImplementation(async (_event, handler) => { invalidate = () => handler({ event: 'kaicut:native-preview-invalidated', id: 1, payload: null }); return unlisten })
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { frames.set(++frameId, callback); return frameId })
  vi.stubGlobal('cancelAnimationFrame', (id: number) => frames.delete(id))
  vi.stubGlobal('ResizeObserver', class {
    constructor(callback: () => void) { resizeCallbacks.push(callback) }
    observe() {}
    disconnect() {}
  })
  vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (this: HTMLElement) {
    if (this.classList.contains('preview-canvas')) return new DOMRect(canvasX, 100, 600, 400)
    if (this.classList.contains('preview-controls')) return new DOMRect(canvasX + 8, 456, 584, 36)
    return new DOMRect(0, 0, 1024, 768)
  })
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
})
afterEach(async () => {
  await act(async () => root.unmount())
  host.remove()
  document.querySelectorAll('[data-test-overlay]').forEach((element) => element.remove())
  vi.restoreAllMocks(); vi.unstubAllGlobals()
})

describe('native preview optional host integration', () => {
  it('does not create a child over the compatibility player when explicitly disabled', async () => {
    await mount(<Fixture enabled={false} />)
    expect(calls('native_preview_host_status')).toHaveLength(0)
    expect(calls('native_preview_host_layout')).toHaveLength(0)
  })

  it('reports host readiness only after its layout has completed', async () => {
    const slow = deferred<unknown>()
    const onStatus = vi.fn()
    mockInvoke.mockImplementation(async (command) => command === 'native_preview_host_status' ? status : slow.promise)
    await mount(<Fixture onStatus={onStatus} />)
    expect(onStatus).not.toHaveBeenCalledWith('attached')
    await act(async () => slow.resolve({}))
    expect(onStatus).toHaveBeenLastCalledWith('attached')
  })

  it('does nothing after disabled or unavailable status', async () => {
    mockInvoke.mockResolvedValueOnce({ enabled: false, sessionId: null, revision: 0 })
    await mount()
    expect(resizeCallbacks).toHaveLength(0)
    expect(listen).not.toHaveBeenCalled()
    expect(calls('native_preview_host_layout')).toHaveLength(0)
    mockInvoke.mockRejectedValueOnce(new Error('unknown command'))
    await act(async () => root.render(<Fixture key="older-backend" />))
    await tick()
    expect(calls('native_preview_host_layout')).toHaveLength(0)
  })

  it('measures content despite hidden document, sends viewport and monotonic ticket', async () => {
    vi.spyOn(document, 'hidden', 'get').mockReturnValue(true)
    await mount()
    const request = calls('native_preview_host_layout')[0][1]?.request
    expect(request).toEqual(expect.objectContaining({ x: 100, y: 100, width: 600, height: 348,
      scaleFactor: window.devicePixelRatio, viewportWidth: window.innerWidth, viewportHeight: window.innerHeight, blocked: false }))
    expect((request as { sequence: number }).sequence).toBeGreaterThan(status.revision)
    expect(host.querySelector('.preview-canvas')?.getAttribute('data-native-preview-host')).toBe('attached')
  })

  it('coalesces slow native calls to the latest layout without concurrent mutations', async () => {
    const slow = deferred<unknown>()
    mockInvoke.mockImplementation(async (command) => command === 'native_preview_host_status' ? status : slow.promise)
    await mount()
    await resize(120); await resize(140); await resize(180)
    expect(calls('native_preview_host_layout')).toHaveLength(1)
    mockInvoke.mockImplementation(async () => ({}))
    await act(async () => slow.resolve({}))
    expect(calls('native_preview_host_layout')).toHaveLength(2)
    expect(calls('native_preview_host_layout')[1][1]?.request).toEqual(expect.objectContaining({ x: 180 }))
  })

  it('hides for menus and errors, resumes on removal, and ignores CSS-hidden overlays', async () => {
    await mount()
    const menu = document.createElement('div'); menu.role = 'menu'; menu.dataset.testOverlay = 'true'
    await act(async () => document.body.append(menu)); await tick()
    expect(calls('native_preview_host_layout').at(-1)?.[1]?.request).toEqual(expect.objectContaining({ blocked: true, width: 0, height: 0 }))
    await act(async () => { menu.style.display = 'none' }); await tick()
    expect(calls('native_preview_host_layout').at(-1)?.[1]?.request).toEqual(expect.objectContaining({ blocked: false }))
    await act(async () => { menu.role = 'alert'; menu.style.display = '' }); await tick()
    expect(calls('native_preview_host_layout').at(-1)?.[1]?.request).toEqual(expect.objectContaining({ blocked: true }))
    await act(async () => menu.remove()); await tick()
    expect(calls('native_preview_host_layout').at(-1)?.[1]?.request).toEqual(expect.objectContaining({ blocked: false }))
  })

  it('places a release after an outstanding call and never adds more layout after failure', async () => {
    const slow = deferred<unknown>()
    mockInvoke.mockImplementation(async (command) => command === 'native_preview_host_status' ? status : command === 'native_preview_host_layout' ? slow.promise : {})
    await mount()
    await resize(130)
    await act(async () => slow.reject(new Error('DPI changed')))
    expect(host.querySelector('.preview-canvas')?.getAttribute('data-native-preview-host')).toBe('fallback')
    expect(calls('native_preview_host_layout')).toHaveLength(1)
    expect(calls('native_preview_host_release')).toHaveLength(1)
    await resize(180)
    expect(calls('native_preview_host_layout')).toHaveLength(1)
  })

  it('defers unmount release until an outstanding mutation finishes', async () => {
    const slow = deferred<unknown>()
    mockInvoke.mockImplementation(async (command) => command === 'native_preview_host_status' ? status : command === 'native_preview_host_layout' ? slow.promise : {})
    await mount()
    await act(async () => root.render(null))
    expect(calls('native_preview_host_release')).toHaveLength(0)
    await act(async () => slow.resolve({}))
    expect(calls('native_preview_host_release')).toHaveLength(1)
    expect(calls('native_preview_host_release')[0][1]?.sequence).toBeGreaterThan((calls('native_preview_host_layout')[0][1]?.request as { sequence: number }).sequence)
  })

  it('suspends for ancestor motion until its completion', async () => {
    await mount()
    await act(async () => host.dispatchEvent(new Event('transitionrun', { bubbles: true }))); await tick()
    expect(calls('native_preview_host_layout').at(-1)?.[1]?.request).toEqual(expect.objectContaining({ blocked: true }))
    await act(async () => host.dispatchEvent(new Event('transitionend', { bubbles: true }))); await tick()
    expect(calls('native_preview_host_layout').at(-1)?.[1]?.request).toEqual(expect.objectContaining({ blocked: false }))
  })

  it('ignores an old response across remount and replaces obsolete release with newer layout', async () => {
    const slow = deferred<unknown>()
    let first = true
    mockInvoke.mockImplementation(async (command) => {
      if (command === 'native_preview_host_status') return status
      if (command === 'native_preview_host_layout' && first) { first = false; return slow.promise }
      return {}
    })
    await mount()
    await act(async () => root.render(<Fixture key="new-owner" />))
    await resize(200)
    expect(calls('native_preview_host_layout')).toHaveLength(1)
    await act(async () => slow.reject(new Error('old surface error')))
    expect(calls('native_preview_host_layout')).toHaveLength(2)
    expect(host.querySelector('.preview-canvas')?.getAttribute('data-native-preview-host')).toBe('attached')
    expect(calls('native_preview_host_release')).toHaveLength(0)
    const requests = calls('native_preview_host_layout').map((entry) => entry[1]?.request as { sequence: number; x: number })
    expect(requests[1].sequence).toBeGreaterThan(requests[0].sequence)
    expect(requests[1].x).toBe(200)
  })

  it('does not let StrictMode stale status create a second host', async () => {
    const old = deferred<unknown>()
    mockInvoke.mockImplementationOnce(() => old.promise)
    await mount(<StrictMode><Fixture /></StrictMode>)
    expect(calls('native_preview_host_status')).toHaveLength(2)
    expect(calls('native_preview_host_layout')).toHaveLength(1)
    await act(async () => old.resolve(status)); await tick()
    expect(calls('native_preview_host_layout')).toHaveLength(1)
  })

  it('resubmits unchanged geometry after a native resize invalidates the host', async () => {
    await mount()
    expect(listen).toHaveBeenCalledWith('kaicut:native-preview-invalidated', expect.any(Function), { target: { kind: 'Window', label: 'main' } })
    await resize(100)
    expect(calls('native_preview_host_layout')).toHaveLength(1)
    await act(async () => invalidate?.()); await tick()
    expect(calls('native_preview_host_layout')).toHaveLength(2)
    const requests = calls('native_preview_host_layout').map((entry) => entry[1]?.request as { sequence: number; x: number })
    expect(requests[1].x).toBe(requests[0].x)
    expect(requests[1].sequence).toBeGreaterThan(requests[0].sequence)
    await act(async () => root.render(null))
    expect(unlisten).toHaveBeenCalledTimes(1)
  })

  it('removes a listener whose registration finishes after unmount', async () => {
    const subscription = deferred<() => void>()
    vi.mocked(listen).mockImplementationOnce(() => subscription.promise)
    await mount()
    expect(calls('native_preview_host_layout')).toHaveLength(0)
    await act(async () => root.render(null))
    await act(async () => subscription.resolve(unlisten))
    expect(unlisten).toHaveBeenCalledTimes(1)
    await tick()
    expect(calls('native_preview_host_layout')).toHaveLength(0)
  })
})
