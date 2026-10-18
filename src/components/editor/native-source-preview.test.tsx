import { act, useEffect } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { invoke } from '@tauri-apps/api/core'
import { PreviewArea, type TransportRequest } from './preview-area'
import { createEmptyProject } from '@/lib/kaicut-format'
import { useProjectStore } from '@/stores/project-store'
import type { MediaAsset } from '@/types/project'

vi.mock('@tauri-apps/api/core', () => ({ isTauri: () => true, invoke: vi.fn(), convertFileSrc: (path: string) => `asset://${path}` }))
vi.mock('@/hooks/use-native-preview-host', () => ({ useNativePreviewHost: (_canvas: unknown, options?: { enabled?: boolean; onStatus?: (status: string) => void }) => {
  const enabled = options?.enabled, onStatus = options?.onStatus
  useEffect(() => { reportHostStatus = onStatus; if (enabled !== false) onStatus?.('attached') }, [enabled, onStatus])
} }))
let reportHostStatus: ((status: string) => void) | undefined
const mockInvoke = vi.mocked(invoke)
const asset: MediaAsset = { id: 'video', name: '素材.mp4', kind: 'video', extension: 'mp4', sourcePath: 'E:/素材.mp4', linkState: 'online', size: 10, lastModified: 1, probe: { state: 'ready', durationMs: 4000 } }
let root: Root
let host: HTMLDivElement
let phase = 'ready'
let positionNs = '0'
const snapshot = () => ({ supported: true, token: 'owned', mediaId: 'video', durationNs: '4000000000', positionNs, phase, muted: false })
function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (reason: Error) => void
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}
const calls = (command: string) => mockInvoke.mock.calls.filter(([name]) => name === command)
beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true })
  vi.useFakeTimers()
  phase = 'ready'; positionNs = '0'
  reportHostStatus = undefined
  mockInvoke.mockReset().mockImplementation(async (command, args) => {
    if (command === 'native_source_capability') return { enabled: true }
    if (command === 'native_source_cancel_open') return { cancelled: true }
    if (command === 'native_source_control') {
      const { action, positionNs: target } = (args as { request: { action: string; positionNs?: string } }).request
      if (action === 'play') { if (phase === 'ended') positionNs = '0'; phase = 'playing' }
      if (action === 'pause') phase = 'paused'
      if (action === 'seek' && target) positionNs = target
    }
    return snapshot()
  })
  vi.spyOn(HTMLMediaElement.prototype, 'pause').mockImplementation(() => {})
  useProjectStore.setState({ project: createEmptyProject(), media: [asset], selectedMediaId: asset.id, selectedClipId: null, selectedClipIds: [], previewMode: 'source', revision: 0, busy: false, ensureThumbnail: vi.fn() })
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
})
afterEach(async () => { await act(async () => root.unmount()); host.remove(); vi.useRealTimers(); vi.restoreAllMocks() })
async function mount() { await act(async () => root.render(<PreviewArea />)) }
async function click(label: string) { await act(async () => host.querySelector<HTMLButtonElement>(`[aria-label="${label}"]`)!.click()) }

describe('optional native source preview controls', () => {
  it('plays the ordinary selected asset without mounting a second media decoder', async () => {
    await mount()
    expect(calls('native_source_capability')).toHaveLength(1)
    expect(host.querySelector('[data-preview-engine="native-source"]')).toBeTruthy()
    expect(host.querySelector('video,audio')).toBeNull()
    expect(host.querySelector('[aria-label="素材时间"]')?.textContent).toContain('00:00:04.000')
    await click('播放')
    expect(host.querySelector('[data-playing="true"]')).toBeTruthy()
    await click('暂停')
    expect(host.querySelector('[data-playing="false"]')).toBeTruthy()
  })

  it('handles source transport requests and exposes EOF with a replay button', async () => {
    await mount()
    await act(async () => new Promise<void>((resolve, reject) => window.dispatchEvent(new CustomEvent<TransportRequest>('kaicut:transport', { detail: { action: 'play', resolve, reject } }))))
    expect(phase).toBe('playing')
    phase = 'ended'; positionNs = '4000000000'
    await act(async () => vi.advanceTimersByTimeAsync(150))
    expect(host.querySelector('[aria-label="播放"]')).toBeTruthy()
    expect(host.textContent).toContain('播放结束')
    await click('播放')
    expect(phase).toBe('playing')
    expect(positionNs).toBe('0')
  })

  it('provides a keyboard-accessible seconds input inside the existing controls', async () => {
    await mount()
    const input = host.querySelector<HTMLInputElement>('[aria-label="素材定位（秒）"]')
    expect(input).toBeTruthy()
    expect(input?.closest('.preview-controls')).toBeTruthy()
    await act(async () => {
      input!.focus()
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, '1.25')
      input!.dispatchEvent(new Event('input', { bubbles: true }))
    })
    await act(async () => input!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })))
    expect(calls('native_source_control').at(-1)?.[1]).toEqual({ request: { token: 'owned', action: 'seek', positionNs: '1250000000' } })
  })

  it.each(['unsupported codec', 'native audio initialization failed'])('uses compatibility preview when open reports %s without a token', async (reason) => {
    mockInvoke.mockImplementation(async (command) => command === 'native_source_capability' ? { enabled: true } : { supported: false, mediaId: 'video', reason })
    await mount()
    expect(host.querySelector('video')).toBeTruthy()
    expect(host.querySelector('[data-preview-engine="native-source"]')).toBeNull()
    expect(host.textContent).toContain('使用兼容预览')
    await act(async () => vi.advanceTimersByTimeAsync(1000))
    expect(calls('native_source_control')).toHaveLength(0)
    expect(calls('native_source_status')).toHaveLength(0)
  })

  it('waits for native close before mounting compatibility media after a decoder error snapshot', async () => {
    await mount()
    const closed = deferred<{ supported: boolean; phase: string }>()
    mockInvoke.mockImplementation(async (command) => {
      if (command === 'native_source_status') return { ...snapshot(), error: 'video decoder exited unexpectedly' }
      return closed.promise
    })
    await act(async () => vi.advanceTimersByTimeAsync(150))
    expect(calls('native_source_control')).toEqual([['native_source_control', { request: { token: 'owned', action: 'close' } }]])
    expect(host.querySelector('[data-preview-engine="native-source"]')).toBeTruthy()
    expect(host.querySelector('video,audio')).toBeNull()
    await act(async () => vi.advanceTimersByTimeAsync(1000))
    expect(calls('native_source_status')).toHaveLength(1)
    expect(host.querySelector('video,audio')).toBeNull()
    await act(async () => closed.resolve({ supported: false, phase: 'closed' }))
    expect(host.querySelector('[data-preview-engine="native-source"]')).toBeNull()
    expect(host.querySelectorAll('video,audio')).toHaveLength(1)
    expect(host.textContent).toContain('使用兼容预览')
  })

  it('never mounts a compatibility decoder if native shutdown failed', async () => {
    await mount()
    mockInvoke.mockRejectedValue(new Error('IPC unavailable'))
    await act(async () => vi.advanceTimersByTimeAsync(150))
    expect(host.querySelector('video,audio')).toBeNull()
    expect(host.querySelector<HTMLButtonElement>('[aria-label="播放"]')?.disabled).toBe(true)
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('预览已停止')
  })

  it('holds compatibility media until cancellation, opening and returned-token closure all finish', async () => {
    const opening = deferred<ReturnType<typeof snapshot>>()
    const cancelled = deferred<{ cancelled: boolean; token: string }>()
    const closed = deferred<{ supported: boolean; phase: string }>()
    mockInvoke.mockImplementation(async (command) => {
      if (command === 'native_source_capability') return { enabled: true }
      if (command === 'native_source_open') return opening.promise
      if (command === 'native_source_cancel_open') return cancelled.promise
      return closed.promise
    })
    await mount()
    await act(async () => reportHostStatus?.('fallback'))
    expect(calls('native_source_cancel_open')).toHaveLength(1)
    expect(host.querySelector('video,audio')).toBeNull()
    expect(host.querySelector<HTMLButtonElement>('[aria-label="播放"]')?.disabled).toBe(true)
    await act(async () => cancelled.resolve({ cancelled: true, token: 'owned' }))
    expect(calls('native_source_control')).toEqual([['native_source_control', { request: { token: 'owned', action: 'close' } }]])
    expect(host.querySelector('video,audio')).toBeNull()
    await act(async () => closed.resolve({ supported: false, phase: 'closed' }))
    expect(host.querySelector('video,audio')).toBeNull()
    await act(async () => opening.resolve(snapshot()))
    expect(host.querySelectorAll('video,audio')).toHaveLength(1)
    expect(host.querySelector('[data-preview-engine="native-source"]')).toBeNull()
    expect(calls('native_source_control')).toHaveLength(1)
  })

  it.each(['cancel', 'close'] as const)('blocks compatibility media when pending initialization %s fails', async (failure) => {
    const opening = deferred<ReturnType<typeof snapshot>>()
    mockInvoke.mockImplementation(async (command) => {
      if (command === 'native_source_capability') return { enabled: true }
      if (command === 'native_source_open') return opening.promise
      if (command === 'native_source_cancel_open') {
        if (failure === 'cancel') throw new Error('cancel IPC unavailable')
        return { cancelled: true, pending: true }
      }
      if (failure === 'close') throw new Error('native resource release failed')
      return { supported: false, phase: 'closed' }
    })
    await mount()
    await act(async () => reportHostStatus?.('fallback'))
    expect(calls('native_source_cancel_open')).toHaveLength(1)
    expect(host.querySelector('video,audio')).toBeNull()
    await act(async () => opening.resolve(snapshot()))
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('预览已停止')
    expect(host.querySelector<HTMLButtonElement>('[aria-label="播放"]')?.disabled).toBe(true)
    expect(host.querySelector('video,audio')).toBeNull()
    expect(calls('native_source_control')).toHaveLength(1)
    await act(async () => vi.advanceTimersByTimeAsync(1000))
    expect(calls('native_source_status')).toHaveLength(0)
    expect(host.querySelector('video,audio')).toBeNull()
  })

  it('does not start compatibility media when unsupported initialization retains failed cleanup ownership', async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === 'native_source_capability') return { enabled: true }
      if (command === 'native_source_open') return { supported: false, mediaId: 'video', token: 'retained', error: 'worker join failed' }
      throw new Error('cleanup ownership retained')
    })
    await mount()
    expect(calls('native_source_control')).toEqual([['native_source_control', { request: { token: 'retained', action: 'close' } }]])
    expect(host.querySelector('video,audio')).toBeNull()
    expect(host.querySelector<HTMLButtonElement>('[aria-label="播放"]')?.disabled).toBe(true)
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('预览已停止')
    await act(async () => vi.advanceTimersByTimeAsync(1000))
    expect(calls('native_source_status')).toHaveLength(0)
    expect(host.querySelector('video,audio')).toBeNull()
  })

  it.each(['audio', 'image'] as const)('retains the compatibility path for %s sources', async (kind) => {
    useProjectStore.setState({ media: [{ ...asset, kind }] })
    await mount()
    expect(calls('native_source_open')).toHaveLength(0)
    expect(host.querySelector('[data-preview-engine="native-source"]')).toBeNull()
  })

  it('leaves the default-off player unchanged when the capability is absent', async () => {
    mockInvoke.mockResolvedValue(undefined)
    await mount()
    expect(host.querySelector('video')).toBeTruthy()
    expect(calls('native_source_open')).toHaveLength(0)
    expect(host.textContent).not.toContain('使用兼容预览')
  })

  it('closes the native source when ordinary navigation returns to the timeline', async () => {
    await mount()
    await click('播放')
    await act(async () => useProjectStore.setState({ previewMode: 'timeline' }))
    expect(calls('native_source_control').at(-1)?.[1]).toEqual({ request: { token: 'owned', action: 'close' } })
    expect(host.querySelector('[data-preview-engine="native-source"]')).toBeNull()
    expect(host.querySelector('[data-preview-mode="timeline"]')).toBeTruthy()
  })

  it('remounts each rapid A-B-A selection and ignores old status and open responses', async () => {
    const other: MediaAsset = { ...asset, id: 'video-b', name: '素材B.mp4', sourcePath: 'E:/素材B.mp4' }
    useProjectStore.setState({ media: [asset, other] })
    const oldStatus = deferred<ReturnType<typeof snapshot>>()
    const bOpen = deferred<ReturnType<typeof snapshot>>()
    const finalOpen = deferred<ReturnType<typeof snapshot>>()
    const first = { ...snapshot(), token: 'first-a', positionNs: '1500000000' }
    const final = { ...snapshot(), token: 'final-a', positionNs: '250000000' }
    let openCount = 0
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === 'native_source_capability') return { enabled: true }
      if (command === 'native_source_open') return [first, bOpen.promise, finalOpen.promise][openCount++]
      if (command === 'native_source_cancel_open') return { cancelled: true }
      if (command === 'native_source_status') return (args as { token: string }).token === 'first-a' ? oldStatus.promise : final
      const request = (args as { request: { action: string } }).request
      return request.action === 'close' ? { supported: false, phase: 'closed' } : { ...final, phase: 'playing' }
    })
    await mount()
    await act(async () => vi.advanceTimersByTimeAsync(150))
    expect(calls('native_source_status')).toEqual([['native_source_status', { token: 'first-a' }]])

    await act(async () => useProjectStore.setState({ selectedMediaId: other.id }))
    let stage = host.querySelector('[data-preview-engine="native-source"]')
    expect(stage?.getAttribute('data-preview-media-id')).toBe(other.id)
    expect(stage?.getAttribute('data-preview-phase')).toBe('starting')
    expect(stage?.getAttribute('data-preview-position-ns')).toBe('0')
    expect(host.querySelector<HTMLButtonElement>('[aria-label="播放"]')?.disabled).toBe(true)
    await act(async () => useProjectStore.setState({ selectedMediaId: asset.id }))
    expect(host.querySelector<HTMLButtonElement>('[aria-label="播放"]')?.disabled).toBe(true)
    expect(calls('native_source_open')).toHaveLength(3)

    await act(async () => finalOpen.resolve(final))
    await act(async () => oldStatus.resolve({ ...first, phase: 'ended', positionNs: '4000000000' }))
    await act(async () => bOpen.resolve({ ...snapshot(), mediaId: other.id, token: 'late-b' }))
    stage = host.querySelector('[data-preview-engine="native-source"]')
    expect(stage?.getAttribute('data-preview-media-id')).toBe(asset.id)
    expect(stage?.getAttribute('data-preview-phase')).toBe('ready')
    expect(stage?.getAttribute('data-preview-position-ns')).toBe(final.positionNs)
    expect(host.querySelector('video,audio')).toBeNull()
    expect(calls('native_source_control')).toEqual([
      ['native_source_control', { request: { token: 'first-a', action: 'close' } }],
      ['native_source_control', { request: { token: 'late-b', action: 'close' } }],
    ])
    await click('播放')
    expect(calls('native_source_control').at(-1)?.[1]).toEqual({ request: { token: 'final-a', action: 'play' } })
    expect(host.querySelector('[data-playing="true"]')).toBeTruthy()
  })

  it.each(['success', 'failure'] as const)('keeps the newly selected source when the old seek returns %s', async (outcome) => {
    const other: MediaAsset = { ...asset, id: 'video-b', name: '素材B.mp4', sourcePath: 'E:/素材B.mp4' }
    const next = { ...snapshot(), mediaId: other.id, token: 'owned-b', positionNs: '250000000' }
    const seeking = deferred<ReturnType<typeof snapshot>>()
    useProjectStore.setState({ media: [asset, other] })
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === 'native_source_capability') return { enabled: true }
      if (command === 'native_source_open') return (args as { request: { mediaId: string } }).request.mediaId === asset.id ? snapshot() : next
      if (command === 'native_source_status') return next
      const request = (args as { request: { action: string } }).request
      if (request.action === 'seek') return seeking.promise
      if (request.action === 'close') return { supported: false, phase: 'closed' }
      return { ...next, phase: 'playing' }
    })
    await mount()
    const input = host.querySelector<HTMLInputElement>('[aria-label="素材定位（秒）"]')!
    await act(async () => {
      input.focus()
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, '1.25')
      input.dispatchEvent(new Event('input', { bubbles: true }))
    })
    await act(async () => input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })))
    expect(calls('native_source_control')).toEqual([['native_source_control', { request: { token: 'owned', action: 'seek', positionNs: '1250000000' } }]])
    await act(async () => useProjectStore.setState({ selectedMediaId: other.id }))
    expect(host.querySelector('[aria-label="素材定位（秒）"]')).not.toBe(input)
    await act(async () => {
      if (outcome === 'success') seeking.resolve({ ...snapshot(), phase: 'paused', positionNs: '1250000000' })
      else seeking.reject(new Error('decoder exited while seeking'))
    })
    await act(async () => vi.advanceTimersByTimeAsync(150))
    const stage = host.querySelector('[data-preview-engine="native-source"]')
    expect(stage?.getAttribute('data-preview-media-id')).toBe(other.id)
    expect(stage?.getAttribute('data-preview-phase')).toBe('ready')
    expect(stage?.getAttribute('data-preview-position-ns')).toBe('250000000')
    expect(host.querySelector('video,audio')).toBeNull()
    expect(host.querySelector('[role="alert"]')).toBeNull()
    expect(calls('native_source_control')).toEqual([
      ['native_source_control', { request: { token: 'owned', action: 'seek', positionNs: '1250000000' } }],
      ['native_source_control', { request: { token: 'owned', action: 'close' } }],
    ])
    expect(calls('native_source_status')).toEqual([['native_source_status', { token: 'owned-b' }]])
    await click('播放')
    expect(calls('native_source_control').at(-1)?.[1]).toEqual({ request: { token: 'owned-b', action: 'play' } })
    expect(host.querySelector('[data-playing="true"]')).toBeTruthy()
  })

  it('reclaims a late opening token after the preparing preview is unmounted', async () => {
    const opening = deferred<ReturnType<typeof snapshot>>()
    mockInvoke.mockImplementation(async (command) => {
      if (command === 'native_source_capability') return { enabled: true }
      if (command === 'native_source_open') return opening.promise
      if (command === 'native_source_cancel_open') return { cancelled: true, pending: true }
      return { supported: false, phase: 'closed' }
    })
    await mount()
    expect(host.querySelector<HTMLButtonElement>('[aria-label="播放"]')?.disabled).toBe(true)
    expect(host.querySelector('video,audio')).toBeNull()
    await click('播放')
    expect(calls('native_source_control')).toHaveLength(0)
    await act(async () => root.render(null))
    const requestId = (calls('native_source_open')[0][1] as { request: { requestId: string } }).request.requestId
    expect(calls('native_source_cancel_open')).toEqual([['native_source_cancel_open', { request: {
      projectId: useProjectStore.getState().project.projectId, revision: 0, requestId,
    } }]])
    await act(async () => opening.resolve({ ...snapshot(), token: 'abandoned' }))
    await act(async () => vi.advanceTimersByTimeAsync(1000))
    expect(calls('native_source_control')).toEqual([['native_source_control', { request: { token: 'abandoned', action: 'close' } }]])
    expect(calls('native_source_status')).toHaveLength(0)
    expect(host.childElementCount).toBe(0)
  })
})
