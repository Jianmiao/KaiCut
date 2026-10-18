import { useCallback, useEffect, useRef, useState } from 'react'
import { invoke, isTauri } from '@tauri-apps/api/core'

export interface NativeSourceSnapshot {
  supported: boolean
  mediaId: string
  token?: string
  durationNs?: string
  positionNs?: string
  phase?: 'ready' | 'playing' | 'paused' | 'ended'
  muted?: boolean
  reason?: string
  error?: string
}
interface SourceIdentity { projectId: string; revision: number; mediaId: string; enabled: boolean; hostFailed?: boolean }
type ControlAction = 'play' | 'pause' | 'seek'
interface SourceState { mode: 'starting' | 'native' | 'fallback' | 'error'; snapshot: NativeSourceSnapshot | null }
interface Waiter { resolve: () => void; reject: (error: Error) => void }
interface PendingControl { action: ControlAction; positionNs?: string; waiters: Waiter[] }
interface SourceClient { control: (action: ControlAction, positionNs?: string) => Promise<void>; stop: () => Promise<void> }
interface CancelOpenResult { cancelled: boolean; pending?: boolean; token?: string }
const unavailable = () => new Error('素材预览尚未准备好，请稍后重试')
const nanoseconds = (value: unknown): value is string => typeof value === 'string' && /^\d+$/.test(value) && value.length <= 20

/** Once per mounted preview, with StrictMode's abandoned effect cancelled. */
export function useNativeSourceCapability() {
  const [enabled, setEnabled] = useState(false)
  useEffect(() => {
    let alive = true
    if (isTauri()) void Promise.resolve().then(async () => {
      if (!alive) return
      try {
        const status = await invoke<{ enabled: boolean }>('native_source_capability')
        if (alive) setEnabled(status?.enabled === true)
      } catch { /* Older and ordinary builds retain their existing transport. */ }
    })
    return () => { alive = false }
  }, [])
  return enabled
}

/** One source owns its token; all status/control requests share a serial lane. */
export function useNativeSourcePreview({ projectId, revision, mediaId, enabled, hostFailed = false }: SourceIdentity) {
  const [state, setState] = useState<SourceState>({ mode: 'starting', snapshot: null })
  const client = useRef<SourceClient | null>(null)
  useEffect(() => {
    if (!enabled) return
    let alive = true
    let stopped = false
    let running = false
    let token: string | undefined
    let snapshot: NativeSourceSnapshot | null = null
    const requestId = crypto.randomUUID()
    let opening = false
    let openResult: Promise<NativeSourceSnapshot | undefined> | undefined
    let cancellation: Promise<CancelOpenResult> | undefined
    const retainedTokens = new Set<string>()
    const closing = new Map<string, { promise: Promise<void>; failed: boolean }>()
    let timer: ReturnType<typeof setTimeout> | undefined
    const pending: PendingControl[] = []
    const current = () => alive && !stopped
    const clearTimer = () => { clearTimeout(timer); timer = undefined }
    const close = (ownedToken: string | undefined, retry = false): Promise<void> => {
      if (!ownedToken) return Promise.resolve()
      const previous = closing.get(ownedToken)
      if (previous && !(retry && previous.failed)) return previous.promise
      retainedTokens.add(ownedToken)
      const attempt = { promise: Promise.resolve(), failed: false }
      attempt.promise = invoke('native_source_control', { request: { token: ownedToken, action: 'close' } }).then(() => {
        retainedTokens.delete(ownedToken)
        if (token === ownedToken) token = undefined
      }, (error: unknown) => { attempt.failed = true; throw error })
      closing.set(ownedToken, attempt)
      return attempt.promise
    }
    const release = async (retry = false) => {
      if (opening) {
        cancellation ??= invoke<CancelOpenResult>('native_source_cancel_open', { request: { projectId, revision, requestId } })
        const cancelled = await cancellation
        if (typeof cancelled?.cancelled !== 'boolean') throw unavailable()
        await close(cancelled.token, retry)
      }
      // Cancellation acknowledges intent, not worker completion. Even an unknown
      // request can have a late response, so settle this effect's open first.
      const opened = await openResult
      await close(opened?.token, retry)
      await close(token, retry)
      for (const retained of retainedTokens) await close(retained, retry)
    }
    const rejectPending = () => { pending.splice(0).forEach((operation) => operation.waiters.forEach(({ reject }) => reject(unavailable()))) }
    const fallback = async () => {
      if (stopped) return
      stopped = true
      clearTimer(); rejectPending()
      try {
        await release()
        if (alive) setState({ mode: 'fallback', snapshot: null })
      } catch {
        // A disappearing child surface does not prove the audio worker exited.
        // Retain ownership for cleanup retry and never start a second decoder.
        if (alive) setState({ mode: 'error', snapshot: null })
      }
    }
    const accept = (next: NativeSourceSnapshot) => {
      if (!next?.supported || next.error || !next.token || next.mediaId !== mediaId || (token && token !== next.token)
        || !nanoseconds(next.durationNs) || !nanoseconds(next.positionNs) || BigInt(next.durationNs) <= 0n
        || BigInt(next.positionNs) > BigInt(next.durationNs) || !['ready', 'playing', 'paused', 'ended'].includes(next.phase ?? '')) throw unavailable()
      snapshot = next
      setState({ mode: 'native', snapshot: next })
    }
    const schedulePoll = () => {
      clearTimer()
      if (current() && token) timer = setTimeout(() => { void drain(true) }, 150)
    }
    const drain = async (poll = false) => {
      if (!current() || !token || running) return
      running = true
      clearTimer()
      try {
        if (poll && pending.length === 0) {
          const next = await invoke<NativeSourceSnapshot>('native_source_status', { token })
          if (current()) accept(next)
        }
        while (current() && pending.length > 0) {
          const operation = pending.shift()!
          try {
            const next = await invoke<NativeSourceSnapshot>('native_source_control', { request: { token, action: operation.action,
              ...(operation.positionNs === undefined ? {} : { positionNs: operation.positionNs }) } })
            if (!current()) throw unavailable()
            accept(next)
            operation.waiters.forEach(({ resolve }) => resolve())
          } catch (error) {
            operation.waiters.forEach(({ reject }) => reject(error instanceof Error ? error : unavailable()))
            throw error
          }
        }
      } catch { if (current()) await fallback() }
      finally { running = false; schedulePoll() }
    }
    const ownedClient: SourceClient = { stop: fallback, control(action, positionNs) {
      if (!current() || !token) return Promise.reject(unavailable())
      if (action === 'seek') {
        if (!nanoseconds(positionNs) || !snapshot?.durationNs) return Promise.reject(new Error('请输入有效的素材时间'))
        positionNs = String(BigInt(positionNs) > BigInt(snapshot.durationNs) ? BigInt(snapshot.durationNs) : BigInt(positionNs))
      }
      return new Promise<void>((resolve, reject) => {
        // At most one seek and one play/pause intent wait behind the active call.
        const superseded = pending.findIndex((operation) => (operation.action === 'seek') === (action === 'seek'))
        const waiters = superseded < 0 ? [] : pending.splice(superseded, 1)[0].waiters
        pending.push({ action, positionNs, waiters: [...waiters, { resolve, reject }] })
        void drain()
      })
    } }
    client.current = ownedClient
    void Promise.resolve().then(async () => {
      if (!current()) return
      try {
        opening = true
        const request = invoke<NativeSourceSnapshot>('native_source_open', { request: { projectId, revision, mediaId, requestId } })
        openResult = request.then((next) => { opening = false; return next }, () => { opening = false; return undefined })
        const next = await request
        if (!current()) { await close(next?.token); return }
        token = next?.token
        accept(next)
        schedulePoll()
      } catch {
        if (current()) await fallback()
        else if (alive) setState({ mode: 'error', snapshot: null })
      }
    })
    return () => {
      alive = false; stopped = true; clearTimer(); rejectPending()
      if (client.current === ownedClient) client.current = null
      void release(true).catch(() => {})
    }
  }, [projectId, revision, mediaId, enabled])
  useEffect(() => {
    if (!hostFailed) return
    let alive = true
    if (client.current) void client.current.stop()
    else void Promise.resolve().then(() => { if (alive) setState({ mode: 'fallback', snapshot: null }) })
    return () => { alive = false }
  }, [hostFailed])
  const control = useCallback((action: ControlAction, positionNs?: string) => client.current?.control(action, positionNs) ?? Promise.reject(unavailable()), [])
  return { ...state, control }
}
