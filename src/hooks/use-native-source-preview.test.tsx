import { act, useEffect } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { invoke } from '@tauri-apps/api/core'
import { useNativeSourcePreview, type NativeSourceSnapshot } from './use-native-source-preview'

vi.mock('@tauri-apps/api/core', () => ({ isTauri: () => true, invoke: vi.fn() }))
const mockInvoke = vi.mocked(invoke)
const initial: NativeSourceSnapshot = { supported: true, mediaId: 'a', token: 'token-a', phase: 'ready', durationNs: '4000000000', positionNs: '0' }
let root: Root
let host: HTMLDivElement
let result: ReturnType<typeof useNativeSourcePreview>
function Fixture({ mediaId = 'a', ready = true, hostFailed = false }: { mediaId?: string; ready?: boolean; hostFailed?: boolean }) {
  const transport = useNativeSourcePreview({ projectId: 'project', revision: 7, mediaId, enabled: ready, hostFailed })
  useEffect(() => { result = transport })
  return <span>{transport.mode}:{transport.snapshot?.phase}</span>
}
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
  mockInvoke.mockReset().mockImplementation(async (command, args) => command === 'native_source_cancel_open' ? { cancelled: true } : command === 'native_source_open'
    ? { ...initial, mediaId: (args as { request: { mediaId: string } }).request.mediaId }
    : initial)
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
})
afterEach(async () => { await act(async () => root.unmount()); host.remove(); vi.useRealTimers() })
async function mount(mediaId = 'a', ready = true) { await act(async () => root.render(<Fixture key={mediaId} mediaId={mediaId} ready={ready} />)) }

describe('native source transport ownership', () => {
  it('waits for its host and opens with project identity, never a frontend path', async () => {
    await mount('a', false)
    expect(calls('native_source_open')).toHaveLength(0)
    await mount()
    expect(calls('native_source_open')).toHaveLength(1)
    expect(calls('native_source_open')[0][1]).toEqual({ request: { projectId: 'project', revision: 7, mediaId: 'a', requestId: expect.any(String) } })
    expect(result.mode).toBe('native')
  })

  it('closes a late old token without replacing the newly selected source', async () => {
    const old = deferred<NativeSourceSnapshot>()
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === 'native_source_cancel_open') return { cancelled: true }
      if (command !== 'native_source_open') return initial
      const id = (args as { request: { mediaId: string } }).request.mediaId
      return id === 'a' ? old.promise : { ...initial, mediaId: 'b', token: 'token-b' }
    })
    await mount('a'); await mount('b')
    await act(async () => old.resolve(initial))
    expect(result.snapshot?.token).toBe('token-b')
    expect(calls('native_source_control')).toContainEqual(['native_source_control', { request: { token: 'token-a', action: 'close' } }])
  })

  it.each([[2, 1, 0], [1, 0, 2]])('owns only the last A during rapid A-B-A opens completing in order %s-%s-%s', async (...order) => {
    const opens = [deferred<NativeSourceSnapshot>(), deferred<NativeSourceSnapshot>(), deferred<NativeSourceSnapshot>()]
    const responses = [
      { ...initial, token: 'token-a-old' },
      { ...initial, mediaId: 'b', token: 'token-b' },
      { ...initial, token: 'token-a-final' },
    ]
    let opened = 0
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === 'native_source_open') return opens[opened++].promise
      if (command === 'native_source_cancel_open') return { cancelled: true }
      const request = (args as { request: { token: string; action: string } }).request
      return request.action === 'close' ? { supported: false, phase: 'closed' } : responses[2]
    })
    await mount('a')
    await expect(result.control('play')).rejects.toThrow()
    await mount('b')
    await expect(result.control('seek', '1000000000')).rejects.toThrow()
    await mount('a')
    expect(result.mode).toBe('starting')
    expect(result.snapshot).toBeNull()
    expect(calls('native_source_control')).toHaveLength(0)
    for (const index of order) await act(async () => opens[index].resolve(responses[index]))
    expect(result.snapshot).toEqual(responses[2])
    const closeRequests = calls('native_source_control').map(([, args]) => args)
    expect(closeRequests).toHaveLength(2)
    expect(closeRequests).toEqual(expect.arrayContaining([
      { request: { token: 'token-a-old', action: 'close' } },
      { request: { token: 'token-b', action: 'close' } },
    ]))
    await act(async () => result.control('play'))
    expect(calls('native_source_control').at(-1)?.[1]).toEqual({ request: { token: 'token-a-final', action: 'play' } })
  })

  it('closes an opening token when the source is unmounted before it is ready', async () => {
    const opening = deferred<NativeSourceSnapshot>()
    mockInvoke.mockImplementation(async (command) => {
      if (command === 'native_source_open') return opening.promise
      if (command === 'native_source_cancel_open') return { cancelled: true }
      if (command === 'native_source_control') return { supported: false, phase: 'closed' }
      return initial
    })
    await mount('a')
    await expect(result.control('play')).rejects.toThrow()
    const abandoned = result
    await act(async () => root.render(null))
    await act(async () => opening.resolve({ ...initial, token: 'token-opening' }))
    expect(calls('native_source_control')).toEqual([['native_source_control', { request: { token: 'token-opening', action: 'close' } }]])
    await expect(abandoned.control('play')).rejects.toThrow()
    await act(async () => vi.advanceTimersByTimeAsync(1000))
    expect(calls('native_source_status')).toHaveLength(0)
    expect(host.textContent).toBe('')
  })

  it('cancels the exact opening request on unmount without cancelling the replacement', async () => {
    const old = deferred<NativeSourceSnapshot>()
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === 'native_source_cancel_open') return { cancelled: true, pending: true }
      if (command === 'native_source_open') return (args as { request: { mediaId: string } }).request.mediaId === 'a'
        ? old.promise : { ...initial, mediaId: 'b', token: 'token-b' }
      return { supported: false, phase: 'closed' }
    })
    await mount('a')
    const requestId = (calls('native_source_open')[0][1] as { request: { requestId: string } }).request.requestId
    await mount('b')
    expect(calls('native_source_cancel_open')).toEqual([['native_source_cancel_open', { request: { projectId: 'project', revision: 7, requestId } }]])
    expect(result.snapshot?.token).toBe('token-b')
    await act(async () => old.resolve({ supported: false, mediaId: 'a', reason: 'cancelled' }))
    expect(result.snapshot?.token).toBe('token-b')
    expect(calls('native_source_control')).toHaveLength(0)
  })

  it('waits for cancelled initialization to finish before host-failure fallback', async () => {
    const opening = deferred<NativeSourceSnapshot>()
    const cancelled = deferred<{ cancelled: boolean; pending: boolean }>()
    mockInvoke.mockImplementation(async (command) => command === 'native_source_open' ? opening.promise : cancelled.promise)
    await mount()
    await act(async () => root.render(<Fixture key="a" hostFailed />))
    expect(calls('native_source_cancel_open')).toHaveLength(1)
    expect(result.mode).toBe('starting')
    await act(async () => cancelled.resolve({ cancelled: true, pending: true }))
    expect(result.mode).toBe('starting')
    await act(async () => opening.resolve({ supported: false, mediaId: 'a', reason: 'cancelled during initialization' }))
    expect(result.mode).toBe('fallback')
    expect(calls('native_source_control')).toHaveLength(0)
  })

  it('reclaims a late cancellation token without changing the newer source', async () => {
    const old = deferred<NativeSourceSnapshot>()
    const cancelled = deferred<{ cancelled: boolean; token: string }>()
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === 'native_source_cancel_open') return cancelled.promise
      if (command === 'native_source_open') return (args as { request: { mediaId: string } }).request.mediaId === 'a'
        ? old.promise : { ...initial, mediaId: 'b', token: 'token-b' }
      return { supported: false, phase: 'closed' }
    })
    await mount('a'); await mount('b')
    await act(async () => cancelled.resolve({ cancelled: true, token: 'late-a' }))
    expect(result.snapshot?.token).toBe('token-b')
    await act(async () => old.resolve({ ...initial, token: 'late-a' }))
    expect(result.snapshot?.token).toBe('token-b')
    expect(calls('native_source_control')).toEqual([['native_source_control', { request: { token: 'late-a', action: 'close' } }]])
  })

  it('closes a token returned by cancellation and awaits the open reply before fallback', async () => {
    const opening = deferred<NativeSourceSnapshot>()
    const closed = deferred<NativeSourceSnapshot>()
    mockInvoke.mockImplementation(async (command) => {
      if (command === 'native_source_open') return opening.promise
      if (command === 'native_source_cancel_open') return { cancelled: true, token: 'token-a' }
      return closed.promise
    })
    await mount()
    await act(async () => root.render(<Fixture key="a" hostFailed />))
    expect(calls('native_source_control')).toEqual([['native_source_control', { request: { token: 'token-a', action: 'close' } }]])
    expect(result.mode).toBe('starting')
    await act(async () => closed.resolve(initial))
    expect(result.mode).toBe('starting')
    await act(async () => opening.resolve(initial))
    expect(result.mode).toBe('fallback')
    expect(calls('native_source_control')).toHaveLength(1)
    expect(calls('native_source_status')).toHaveLength(0)
  })

  it('waits for late open ownership to close even after cancellation reports no pending request', async () => {
    const opening = deferred<NativeSourceSnapshot>()
    const closed = deferred<NativeSourceSnapshot>()
    mockInvoke.mockImplementation(async (command) => {
      if (command === 'native_source_open') return opening.promise
      if (command === 'native_source_cancel_open') return { cancelled: false }
      return closed.promise
    })
    await mount()
    await act(async () => root.render(<Fixture key="a" hostFailed />))
    expect(result.mode).toBe('starting')
    await act(async () => opening.resolve(initial))
    expect(result.mode).toBe('starting')
    expect(calls('native_source_control')).toHaveLength(1)
    await act(async () => closed.resolve(initial))
    expect(result.mode).toBe('fallback')
  })

  it('fails closed when cancellation IPC fails, while still reclaiming a late open token', async () => {
    const opening = deferred<NativeSourceSnapshot>()
    mockInvoke.mockImplementation(async (command) => {
      if (command === 'native_source_open') return opening.promise
      if (command === 'native_source_cancel_open') throw new Error('cancel IPC unavailable')
      return { supported: false, phase: 'closed' }
    })
    await mount()
    await act(async () => root.render(<Fixture key="a" hostFailed />))
    expect(result.mode).toBe('error')
    await act(async () => opening.resolve(initial))
    expect(result.mode).toBe('error')
    expect(calls('native_source_control')).toEqual([['native_source_control', { request: { token: 'token-a', action: 'close' } }]])
    await expect(result.control('play')).rejects.toThrow()
  })

  it('fails closed and retries retained late-open ownership on unmount if cleanup fails', async () => {
    const opening = deferred<NativeSourceSnapshot>()
    let closeAttempts = 0
    mockInvoke.mockImplementation(async (command) => {
      if (command === 'native_source_open') return opening.promise
      if (command === 'native_source_cancel_open') return { cancelled: true, pending: true }
      if (++closeAttempts === 1) throw new Error('worker join failed')
      return { supported: false, phase: 'closed' }
    })
    await mount()
    await act(async () => root.render(<Fixture key="a" hostFailed />))
    await act(async () => opening.resolve(initial))
    expect(result.mode).toBe('error')
    expect(closeAttempts).toBe(1)
    await act(async () => root.render(null))
    expect(closeAttempts).toBe(2)
    expect(calls('native_source_control')).toEqual([
      ['native_source_control', { request: { token: 'token-a', action: 'close' } }],
      ['native_source_control', { request: { token: 'token-a', action: 'close' } }],
    ])
  })

  it('serializes commands and retains only the latest pending seek', async () => {
    await mount()
    const play = deferred<NativeSourceSnapshot>()
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === 'native_source_control' && (args as { request: { action: string } }).request.action === 'play') return play.promise
      return { ...initial, positionNs: '3000000000' }
    })
    let requests: Promise<void>[] = []
    await act(async () => { requests = [result.control('play'), result.control('seek', '1000000000'), result.control('seek', '2000000000'), result.control('seek', '3000000000')] })
    expect(calls('native_source_control')).toHaveLength(1)
    await act(async () => { play.resolve({ ...initial, phase: 'playing' }); await Promise.all(requests) })
    expect(calls('native_source_control')).toHaveLength(2)
    expect(calls('native_source_control')[1][1]).toEqual({ request: { token: 'token-a', action: 'seek', positionNs: '3000000000' } })
  })

  it.each(['success', 'failure'] as const)('rejects abandoned seeks after unmount even when the active seek returns %s', async (outcome) => {
    await mount()
    const seeking = deferred<NativeSourceSnapshot>()
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === 'native_source_control' && (args as { request: { action: string } }).request.action === 'seek') return seeking.promise
      return { supported: false, phase: 'closed' }
    })
    const abandoned = result
    let inFlight!: Promise<unknown>
    let queued!: Promise<unknown>
    await act(async () => {
      inFlight = result.control('seek', '1000000000').then(() => 'accepted', (error: unknown) => error)
      queued = result.control('seek', '2000000000').then(() => 'accepted', (error: unknown) => error)
    })
    await act(async () => root.render(null))
    expect(await queued).toBeInstanceOf(Error)
    await act(async () => {
      if (outcome === 'success') seeking.resolve({ ...initial, phase: 'paused', positionNs: '1000000000' })
      else seeking.reject(new Error('decoder exited while seeking'))
      expect(await inFlight).toBeInstanceOf(Error)
    })
    await expect(abandoned.control('play')).rejects.toThrow()
    await act(async () => vi.advanceTimersByTimeAsync(1000))
    expect(calls('native_source_control')).toEqual([
      ['native_source_control', { request: { token: 'token-a', action: 'seek', positionNs: '1000000000' } }],
      ['native_source_control', { request: { token: 'token-a', action: 'close' } }],
    ])
    expect(calls('native_source_status')).toHaveLength(0)
    expect(host.childElementCount).toBe(0)
  })

  it('polls without overlapping requests and exposes the actual end state', async () => {
    await mount()
    const status = deferred<NativeSourceSnapshot>()
    mockInvoke.mockImplementation(async () => status.promise)
    await act(async () => vi.advanceTimersByTimeAsync(150))
    await act(async () => vi.advanceTimersByTimeAsync(1500))
    expect(calls('native_source_status')).toHaveLength(1)
    await act(async () => status.resolve({ ...initial, phase: 'ended', positionNs: initial.durationNs }))
    expect(result.snapshot?.phase).toBe('ended')
  })

  it('waits for an outstanding status read before sending a user control', async () => {
    await mount()
    const status = deferred<NativeSourceSnapshot>()
    mockInvoke.mockImplementation(async (command) => command === 'native_source_status' ? status.promise : { ...initial, phase: 'paused' })
    await act(async () => vi.advanceTimersByTimeAsync(150))
    let pause!: Promise<void>
    await act(async () => { pause = result.control('pause') })
    expect(calls('native_source_control')).toHaveLength(0)
    await act(async () => { status.resolve({ ...initial, phase: 'playing' }); await pause })
    expect(result.snapshot?.phase).toBe('paused')
  })

  it('does not enter fallback until the current native token has closed', async () => {
    await mount()
    const closed = deferred<NativeSourceSnapshot>()
    mockInvoke.mockImplementation(async (command) => {
      if (command === 'native_source_status') throw new Error('decoder stopped')
      return closed.promise
    })
    await act(async () => vi.advanceTimersByTimeAsync(150))
    expect(result.mode).toBe('native')
    await act(async () => closed.resolve(initial))
    expect(result.mode).toBe('fallback')
  })

  it('ignores a late status update after a source switch', async () => {
    await mount()
    const status = deferred<NativeSourceSnapshot>()
    mockInvoke.mockImplementation(async (command) => command === 'native_source_status' ? status.promise : { ...initial, mediaId: 'b', token: 'token-b' })
    await act(async () => vi.advanceTimersByTimeAsync(150))
    await mount('b')
    await act(async () => status.resolve({ ...initial, phase: 'ended' }))
    expect(result.snapshot?.token).toBe('token-b')
    expect(result.snapshot?.phase).toBe('ready')
  })

  it('closes the owned token before falling back on a backend failure', async () => {
    await mount()
    mockInvoke.mockImplementation(async (command) => {
      if (command === 'native_source_status') throw new Error('decoder failed')
      return initial
    })
    await act(async () => vi.advanceTimersByTimeAsync(150))
    expect(result.mode).toBe('fallback')
    expect(calls('native_source_control').at(-1)?.[1]).toEqual({ request: { token: 'token-a', action: 'close' } })
  })

  it('fails closed if native shutdown cannot be confirmed', async () => {
    await mount()
    mockInvoke.mockRejectedValue(new Error('IPC unavailable'))
    await act(async () => vi.advanceTimersByTimeAsync(150))
    expect(result.mode).toBe('error')
    await expect(result.control('play')).rejects.toThrow()
  })

  it('fails closed when an unsupported open retains a token whose cleanup failed', async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === 'native_source_open') return { supported: false, mediaId: 'a', token: 'retained-a', error: 'worker join failed' }
      throw new Error('cleanup ownership retained')
    })
    await mount()
    expect(result.mode).toBe('error')
    expect(result.snapshot).toBeNull()
    expect(calls('native_source_control')).toEqual([['native_source_control', { request: { token: 'retained-a', action: 'close' } }]])
    await expect(result.control('play')).rejects.toThrow()
    await act(async () => vi.advanceTimersByTimeAsync(1000))
    expect(calls('native_source_status')).toHaveLength(0)
  })

  it('waits for confirmed source shutdown when the host fails', async () => {
    await mount()
    const closed = deferred<NativeSourceSnapshot>()
    mockInvoke.mockImplementation(async () => closed.promise)
    await act(async () => root.render(<Fixture key="a" hostFailed />))
    expect(result.mode).toBe('native')
    expect(calls('native_source_control').at(-1)?.[1]).toEqual({ request: { token: 'token-a', action: 'close' } })
    await act(async () => closed.resolve(initial))
    expect(result.mode).toBe('fallback')
  })

  it.each(['unsupported codec', 'native audio initialization failed'])('falls back without a token after open reports %s', async (reason) => {
    mockInvoke.mockResolvedValue({ supported: false, mediaId: 'a', reason })
    await mount()
    expect(result.mode).toBe('fallback')
    expect(result.snapshot).toBeNull()
    await expect(result.control('play')).rejects.toThrow()
    await act(async () => vi.advanceTimersByTimeAsync(1000))
    expect(calls('native_source_status')).toHaveLength(0)
    expect(calls('native_source_control')).toHaveLength(0)
  })
})
