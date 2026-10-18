import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest'
import { inspectRenderedUi, useMcpBridge } from './use-mcp-bridge'
import { useProjectStore } from '@/stores/project-store'
import { createEmptyProject } from '@/lib/kaicut-format'

const mocks = vi.hoisted(() => ({ invoke: vi.fn(), handlers: new Map<string, (event: { payload: unknown }) => void>() }))
vi.mock('@tauri-apps/api/core', () => ({ isTauri: () => true, invoke: mocks.invoke }))
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async (name: string, callback: (event: { payload: unknown }) => void) => {
  mocks.handlers.set(name, callback)
  return () => mocks.handlers.delete(name)
}) }))
let root: Root
let host: HTMLDivElement
function Bridge() { useMcpBridge(); return <div data-preview-mode="timeline" data-playing="false" data-preview-frame="0" data-preview-visual="" /> }
const send = async (payload: unknown) => {
  await act(async () => { mocks.handlers.get('kaicut:mcp-command')?.({ payload }); await vi.advanceTimersByTimeAsync(40) })
}
beforeEach(async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true })
  vi.useFakeTimers(); mocks.invoke.mockReset().mockResolvedValue(undefined); mocks.handlers.clear()
  useProjectStore.setState({ project: createEmptyProject(), revision: 7, busy: false, isDirty: false, playheadFrame: 0, previewMode: 'timeline', selectedClipIds: [], selectedClipId: null, selectedMediaId: null })
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
  await act(async () => root.render(<Bridge />))
})
afterEach(async () => { await act(async () => root.unmount()); host.remove(); vi.useRealTimers() })
describe('MCP UI projection', () => {
  it('reports the actual transport phase while preserving missing or unknown values as null', () => {
    const preview = host.querySelector<HTMLElement>('[data-preview-mode]')!
    expect(inspectRenderedUi().preview).toHaveProperty('phase', null)
    const before = useProjectStore.getState()
    for (const phase of ['preparing', 'starting', 'playing', 'buffering', 'paused', 'source']) {
      preview.dataset.previewPhase = phase
      expect(inspectRenderedUi().preview).toHaveProperty('phase', phase)
      // Phase is a direct observation, not inferred from the existing playing flag.
      expect(inspectRenderedUi().preview.playing).toBe(false)
    }
    for (const phase of ['', 'ready', 'failed', 'null', 'PLAYING']) {
      preview.dataset.previewPhase = phase
      expect(inspectRenderedUi().preview).toHaveProperty('phase', null)
    }
    preview.removeAttribute('data-preview-mode')
    expect(inspectRenderedUi().preview).toHaveProperty('phase', null)
    expect(useProjectStore.getState()).toBe(before)
  })
  it('reads transport master and buffering telemetry without inventing a ready clock', () => {
    const preview = host.querySelector<HTMLElement>('[data-preview-mode]')!
    expect(inspectRenderedUi().preview).toEqual(expect.objectContaining({ clockMode: null, masterClipId: null, masterKind: null, buffering: null }))
    preview.dataset.previewClock = 'audio'; preview.dataset.previewClockClipId = 'clip-audio'; preview.dataset.previewBuffering = 'true'
    expect(inspectRenderedUi().preview).toEqual(expect.objectContaining({ clockMode: 'audio', masterKind: 'audio', masterClipId: 'clip-audio', buffering: true }))
    preview.dataset.previewClock = 'monotonic'; preview.dataset.previewClockClipId = ''; preview.dataset.previewBuffering = 'false'
    expect(inspectRenderedUi().preview).toEqual(expect.objectContaining({ clockMode: 'monotonic', masterKind: null, masterClipId: null, buffering: false }))
    preview.dataset.previewClock = 'source'
    expect(inspectRenderedUi().preview.clockMode).toBe('source')
    preview.dataset.previewClock = 'unknown'; preview.dataset.previewBuffering = 'unknown'
    expect(inspectRenderedUi().preview).toEqual(expect.objectContaining({ clockMode: null, buffering: null }))
  })
  it('reports measured playback rate and element-lifetime hard seek count with unavailable values kept null', () => {
    const audio = document.createElement('audio'); audio.playbackRate = 1
    const video = document.createElement('video'); video.playbackRate = 0.98
    video.dataset.previewHardSeeks = '3'
    host.append(audio, video)
    expect(inspectRenderedUi().mediaNodes).toEqual([
      expect.objectContaining({ kind: 'audio', playbackRate: 1, hardSeekCount: null }),
      expect.objectContaining({ kind: 'video', playbackRate: 0.98, hardSeekCount: 3 }),
    ])
    video.dataset.previewHardSeeks = '0'
    expect(inspectRenderedUi().mediaNodes[1].hardSeekCount).toBe(0)
    for (const value of ['', '-1', '1.5', 'Infinity', 'NaN', '9007199254740992']) {
      video.dataset.previewHardSeeks = value
      expect(inspectRenderedUi().mediaNodes[1].hardSeekCount).toBeNull()
    }
    Object.defineProperty(video, 'playbackRate', { value: Number.NaN })
    expect(inspectRenderedUi().mediaNodes[1].playbackRate).toBeNull()
  })
  it('preserves the supplied media element identity across clip reuse and exposes replacement identity', () => {
    const video = document.createElement('video'); host.append(video)
    expect(inspectRenderedUi().mediaNodes[0].nodeId).toBeNull()
    video.dataset.previewNodeId = 'node-one'; video.dataset.previewClipId = 'clip-a'; video.dataset.previewHardSeeks = '1'
    expect(inspectRenderedUi().mediaNodes[0]).toEqual(expect.objectContaining({ nodeId: 'node-one', clipId: 'clip-a', hardSeekCount: 1 }))
    video.dataset.previewClipId = 'clip-b'
    expect(inspectRenderedUi().mediaNodes[0]).toEqual(expect.objectContaining({ nodeId: 'node-one', clipId: 'clip-b', hardSeekCount: 1 }))
    const replacement = document.createElement('video'); replacement.dataset.previewNodeId = 'node-two'; replacement.dataset.previewClipId = 'clip-b'
    video.replaceWith(replacement)
    expect(inspectRenderedUi().mediaNodes[0]).toEqual(expect.objectContaining({ nodeId: 'node-two', clipId: 'clip-b' }))
  })
  it('reports rational timeline source offset and signed media clock drift without changing playback', () => {
    const project = createEmptyProject()
    project.timebase = { fpsNumerator: 30000, fpsDenominator: 1001 }
    project.tracks[0].clips = [{ id: 'clip-video', mediaId: 'video', trackId: project.tracks[0].id, startFrame: 60, sourceInFrame: 30, durationFrames: 120, sourceDurationFrames: 180 }]
    useProjectStore.setState({ project, playheadFrame: 90, previewMode: 'timeline' })
    const video = document.createElement('video')
    video.dataset.previewClipId = 'clip-video'; video.dataset.previewMediaId = 'video'; video.currentTime = 2.052
    const pause = vi.spyOn(video, 'pause')
    host.append(video)
    const before = useProjectStore.getState()
    const node = inspectRenderedUi().mediaNodes[0]
    expect(node.expectedSourceTime).toBeCloseTo(2.002, 8)
    expect(node.driftSeconds).toBeCloseTo(0.05, 8)
    expect(node.currentTime).toBe(2.052)
    expect(useProjectStore.getState()).toBe(before)
    expect(pause).not.toHaveBeenCalled()
  })
  it('does not invent timeline clock drift for source preview or an unknown clip', () => {
    const project = createEmptyProject()
    project.tracks[2].clips = [{ id: 'clip-audio', mediaId: 'audio', trackId: project.tracks[2].id, startFrame: 0, sourceInFrame: 0, durationFrames: 120, sourceDurationFrames: 120 }]
    const audio = document.createElement('audio'); audio.currentTime = 3
    audio.dataset.previewClipId = 'clip-audio'; host.append(audio)
    useProjectStore.setState({ project, previewMode: 'source' })
    expect(inspectRenderedUi().mediaNodes[0]).toEqual(expect.objectContaining({ expectedSourceTime: null, driftSeconds: null, totalVideoFrames: null, droppedVideoFrames: null }))
    audio.dataset.previewClipId = 'not-in-project'
    useProjectStore.setState({ previewMode: 'timeline' })
    expect(inspectRenderedUi().mediaNodes[0]).toEqual(expect.objectContaining({ expectedSourceTime: null, driftSeconds: null }))
  })
  it('samples optional native video quality counters and tolerates unsupported or invalid counters', () => {
    const video = document.createElement('video')
    host.append(video)
    Object.defineProperty(video, 'getVideoPlaybackQuality', { configurable: true, value: undefined })
    expect(inspectRenderedUi().mediaNodes[0]).toEqual(expect.objectContaining({ totalVideoFrames: null, droppedVideoFrames: null }))
    Object.defineProperty(video, 'getVideoPlaybackQuality', { configurable: true, value: () => ({ totalVideoFrames: 120, droppedVideoFrames: 3 }) })
    expect(inspectRenderedUi().mediaNodes[0]).toEqual(expect.objectContaining({ totalVideoFrames: 120, droppedVideoFrames: 3 }))
    Object.defineProperty(video, 'getVideoPlaybackQuality', { configurable: true, value: () => ({ totalVideoFrames: Number.NaN, droppedVideoFrames: Infinity }) })
    expect(inspectRenderedUi().mediaNodes[0]).toEqual(expect.objectContaining({ totalVideoFrames: null, droppedVideoFrames: null }))
    Object.defineProperty(video, 'getVideoPlaybackQuality', { configurable: true, value: () => { throw new Error('unsupported') } })
    expect(() => inspectRenderedUi()).not.toThrow()
    expect(inspectRenderedUi().mediaNodes[0].totalVideoFrames).toBeNull()
  })
  it('returns null clock diagnostics for non-finite metadata and samples only current DOM counts', () => {
    const project = createEmptyProject()
    project.timebase = { fpsNumerator: 0, fpsDenominator: 1 }
    project.tracks[0].clips = [{ id: 'clip-video', mediaId: 'video', trackId: project.tracks[0].id, startFrame: 0, sourceInFrame: 0, durationFrames: 120, sourceDurationFrames: 120 }]
    useProjectStore.setState({ project })
    const video = document.createElement('video'); video.dataset.previewClipId = 'clip-video'
    Object.defineProperty(video, 'currentTime', { value: Number.NaN })
    host.append(video)
    const first = document.createElement('div'); first.className = 'timeline-clip'
    const child = document.createElement('button'); child.className = 'timeline-clip-body'; first.append(child)
    const second = document.createElement('div'); second.className = 'timeline-clip'
    host.append(first, second)
    const before = inspectRenderedUi()
    expect(before.mediaNodes[0]).toEqual(expect.objectContaining({ currentTime: null, expectedSourceTime: null, driftSeconds: null }))
    expect(before.performance.timelineClipElementCount).toBe(2)
    second.remove()
    const after = inspectRenderedUi()
    expect(after.performance.timelineClipElementCount).toBe(1)
    expect(after.performance.domNodeCount).toBe(before.performance.domNodeCount - 1)
    expect(Number.isFinite(after.performance.inspectedAtMonotonicMs)).toBe(true)
    expect(after.performance.inspectionDurationMs).toBeGreaterThanOrEqual(0)
  })
  it('acknowledges a fresh inspection with runtime and rendered evidence', async () => {
    await send({ requestId: 'read', action: 'inspect', args: {} })
    expect(mocks.invoke).toHaveBeenCalledWith('mcp_ui_ack', { requestId: 'read', result: expect.objectContaining({ revision: 7, playheadFrame: 0, preview: expect.objectContaining({ playing: false, mode: 'timeline' }), mediaNodes: [] }) })
  })
  it('rejects stale UI commands instead of executing them in another project', async () => {
    await send({ requestId: 'seek', action: 'seek', args: { projectId: 'old-project', revision: 7, frame: 30 } })
    expect(useProjectStore.getState().playheadFrame).toBe(0)
    expect(mocks.invoke).toHaveBeenCalledWith('mcp_ui_ack', { requestId: 'seek', result: expect.objectContaining({ error: expect.stringContaining('已更新') }) })
  })
  it('preserves project data when seeking and blocks controls behind a modal', async () => {
    const state = useProjectStore.getState()
    await send({ requestId: 'seek', action: 'seek', args: { projectId: state.project.projectId, revision: 7, frame: 30 } })
    expect(useProjectStore.getState().playheadFrame).toBe(30)
    expect(useProjectStore.getState().project).toBe(state.project)
    expect(useProjectStore.getState().isDirty).toBe(false)
    const modal = document.createElement('div'); modal.setAttribute('aria-modal', 'true'); host.append(modal)
    await send({ requestId: 'blocked', action: 'seek', args: { projectId: state.project.projectId, revision: 7, frame: 0 } })
    expect(useProjectStore.getState().playheadFrame).toBe(30)
    expect(mocks.invoke).toHaveBeenLastCalledWith('mcp_ui_ack', { requestId: 'blocked', result: expect.objectContaining({ error: expect.stringContaining('对话框') }) })
  })
  it('updates the UI only from current authoritative native sessions', async () => {
    const state = useProjectStore.getState()
    const session = { project: { ...state.project, name: 'MCP edit' }, revision: 8, path: null, dirty: true, status: '已更新' }
    await act(async () => mocks.handlers.get('kaicut:mcp-session')?.({ payload: session }))
    expect(useProjectStore.getState().project.name).toBe('MCP edit')
    await act(async () => mocks.handlers.get('kaicut:mcp-session')?.({ payload: { ...session, project: state.project, revision: 6 } }))
    expect(useProjectStore.getState().revision).toBe(8)
    const switched = createEmptyProject('另一个项目')
    useProjectStore.setState({ project: switched, revision: 9 })
    await act(async () => mocks.handlers.get('kaicut:mcp-session')?.({ payload: session }))
    expect(useProjectStore.getState().project.projectId).toBe(switched.projectId)
  })
})
