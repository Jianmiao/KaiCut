import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { PreviewArea, type TransportRequest } from './preview-area'
import { useProjectStore } from '@/stores/project-store'
import { createEmptyProject, parseProject } from '@/lib/kaicut-format'
import fixture from '@/test/fixtures/preview-parity.json'
import type { MediaAsset } from '@/types/project'

vi.mock('@tauri-apps/api/core', () => ({ isTauri: () => true, invoke: vi.fn(), convertFileSrc: (path: string) => `asset://localhost/${encodeURIComponent(path)}` }))
let host: HTMLDivElement
let root: Root
let now: number
let nextFrame: number
let raf: Map<number, FrameRequestCallback>
let paused: WeakMap<HTMLMediaElement, boolean>
const ensure = vi.fn()
const retry = vi.fn()
const asset: MediaAsset = { id: 'v', name: '中文 片段.mp4', kind: 'video', extension: 'mp4', size: 100, lastModified: 200, sourcePath: 'E:/素材.mp4', linkState: 'online', probe: { state: 'ready', durationMs: 2501 } }

function pendingPlay() {
  let resolve!: () => void
  let reject!: (error: Error) => void
  const promise = new Promise<void>((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}

async function click(label: string) {
  const button = host.querySelector<HTMLButtonElement>(`[aria-label="${label}"]`)
  expect(button).not.toBeNull()
  await act(async () => button!.click())
}

async function advance(milliseconds: number) {
  const end = now + milliseconds
  while (now < end) {
    const step = Math.min(10, end - now)
    for (const node of host.querySelectorAll<HTMLMediaElement>('video,audio')) if (!node.paused && !node.seeking && node.readyState >= 3) node.currentTime += step / 1000 * node.playbackRate
    await advanceClock(step)
  }
}

async function advanceClock(milliseconds: number) {
  now += milliseconds
  const callbacks = [...raf.values()]
  raf.clear()
  await act(async () => { callbacks.forEach((callback) => callback(now)) })
}

async function timeline(frame = 0) {
  const project = parseProject(JSON.stringify(fixture.project))
  project.timebase = { fpsNumerator: 30, fpsDenominator: 1 }
  const media: MediaAsset[] = project.mediaRefs.map((item) => ({ ...item, linkState: 'online', probe: { state: 'ready', durationMs: 2501 } }))
  await act(async () => useProjectStore.setState({ project, media, previewMode: 'timeline', playheadFrame: frame, selectedMediaId: 'upper', selectedClipId: 'upper-clip', selectedClipIds: ['upper-clip'] }))
  return { project, media }
}

beforeEach(async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true })
  ensure.mockClear(); retry.mockClear()
  now = 0; nextFrame = 0; raf = new Map(); paused = new WeakMap()
  vi.spyOn(performance, 'now').mockImplementation(() => now)
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { const id = ++nextFrame; raf.set(id, callback); return id })
  vi.stubGlobal('cancelAnimationFrame', (id: number) => { raf.delete(id) })
  vi.spyOn(HTMLMediaElement.prototype, 'paused', 'get').mockImplementation(function (this: HTMLMediaElement) { return paused.get(this) ?? true })
  vi.spyOn(HTMLMediaElement.prototype, 'readyState', 'get').mockReturnValue(4)
  vi.spyOn(HTMLMediaElement.prototype, 'play').mockImplementation(function (this: HTMLMediaElement) { paused.set(this, false); return Promise.resolve() })
  vi.spyOn(HTMLMediaElement.prototype, 'pause').mockImplementation(function (this: HTMLMediaElement) { paused.set(this, true) })
  useProjectStore.setState({ project: createEmptyProject(), media: [asset], previewMode: 'source', selectedMediaId: asset.id, selectedClipId: null, selectedClipIds: [], revision: 0, playheadFrame: 0, busy: false, ensureThumbnail: ensure, retryThumbnail: retry })
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
  await act(async () => root.render(<PreviewArea />))
})
afterEach(async () => {
  await act(async () => root.unmount())
  host.remove()
  vi.restoreAllMocks(); vi.unstubAllGlobals()
})

describe('source preview', () => {
  it('plays the selected source and displays its duration without a static-frame claim', async () => {
    expect(host.querySelector('video')).toBeTruthy()
    expect(host.querySelector('[aria-label="素材时间"]')?.textContent).toContain('00:00:02.501')
    expect(host.textContent).not.toContain('静态首帧')
    await click('播放')
    const video = host.querySelector('video')!
    expect(video.paused).toBe(false)
    await act(async () => { video.currentTime = 1.25; video.dispatchEvent(new Event('timeupdate')) })
    expect(host.querySelector('[aria-label="素材时间"]')?.textContent).toContain('00:00:01.250')
    expect(useProjectStore.getState().playheadFrame).toBe(0)
    await click('暂停')
    expect(video.paused).toBe(true)
  })

  it('does not substitute timeline clips while explicitly previewing a source', async () => {
    const { project, media } = await timeline(30)
    await act(async () => useProjectStore.setState({ project, media, previewMode: 'source', selectedMediaId: 'lower' }))
    expect(host.querySelector('video')?.dataset.previewMediaId).toBe('lower')
    expect(host.querySelector('video')?.currentTime).toBe(0)
    expect(host.querySelector('img')).toBeNull()
    expect(host.querySelector('audio')).toBeNull()
  })

  it('plays audio sources with an audio label and no video node', async () => {
    await act(async () => useProjectStore.setState({ media: [{ ...asset, kind: 'audio' }] }))
    expect(host.querySelector('video')).toBeNull()
    expect(host.querySelector('audio')).toBeTruthy()
    expect(host.textContent).toContain(asset.name)
    await click('播放')
    expect(host.querySelector('audio')?.paused).toBe(false)
    await act(async () => host.querySelector('audio')!.dispatchEvent(new Event('ended')))
    expect(host.querySelector('[aria-label="播放"]')).toBeTruthy()
  })

  it.each(['missing', 'changed'] as const)('hides stale source frames when media is %s', async (linkState) => {
    await act(async () => useProjectStore.setState({ media: [{ ...asset, linkState }] }))
    expect(host.querySelector('video')).toBeNull()
    expect(host.textContent).toContain('重新链接')
    expect(host.querySelector<HTMLButtonElement>('[aria-label="播放"]')?.disabled).toBe(true)
  })

  it('retries a decode failure without pretending thumbnail errors are video errors', async () => {
    await act(async () => host.querySelector('video')!.dispatchEvent(new Event('error')))
    expect(host.querySelector('video')).toBeNull()
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('媒体无法播放')
    const reread = [...host.querySelectorAll('button')].find((button) => button.textContent === '重新读取')!
    await act(async () => reread.click())
    expect(host.querySelector('video')).toBeTruthy()
    expect(host.querySelector('[role="alert"]')).toBeNull()
    expect(retry).not.toHaveBeenCalled()
  })

  it('requests image thumbnails again when a different project reuses an asset id', async () => {
    await act(async () => useProjectStore.setState({ media: [{ ...asset, kind: 'image', name: '封面.png' }] }))
    expect(ensure).toHaveBeenCalledTimes(1)
    expect(host.querySelector('img')).toBeTruthy()
    expect(host.querySelector<HTMLButtonElement>('[aria-label="播放"]')?.disabled).toBe(true)
    await act(async () => useProjectStore.setState({ project: createEmptyProject() }))
    expect(ensure).toHaveBeenCalledTimes(2)
  })

  it('shows an active play rejection and stops the transport', async () => {
    vi.mocked(HTMLMediaElement.prototype.play).mockRejectedValueOnce(new Error('Unsupported codec'))
    await click('播放')
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('无法播放')
    expect(host.querySelector('.preview-stage')?.getAttribute('data-playing')).toBe('false')
  })

  it('ignores a late play rejection from a source that has been replaced', async () => {
    const pending = pendingPlay()
    vi.mocked(HTMLMediaElement.prototype.play).mockImplementationOnce(() => pending.promise)
    await click('播放')
    const old = host.querySelector('video')!
    const next = { ...asset, id: 'next', sourcePath: 'E:/新视频.mp4' }
    await act(async () => useProjectStore.setState({ media: [next], selectedMediaId: next.id }))
    await act(async () => pending.reject(new Error('Old decoder failed')))
    expect(old.paused).toBe(true)
    expect(host.querySelector('video')?.dataset.previewMediaId).toBe('next')
    expect(host.querySelector('[role="alert"]')).toBeNull()
    expect(host.querySelector<HTMLButtonElement>('[aria-label="播放"]')?.disabled).toBe(false)
  })

  it('pauses a delayed successful play after the user has paused', async () => {
    const pending = pendingPlay()
    const video = host.querySelector('video')!
    vi.mocked(HTMLMediaElement.prototype.play).mockImplementationOnce(() => pending.promise)
    await click('播放')
    await click('暂停')
    await act(async () => { paused.set(video, false); pending.resolve() })
    expect(video.paused).toBe(true)
    expect(host.querySelector('[role="alert"]')).toBeNull()
  })
})

describe('timeline preview', () => {
  it('waits for current-generation master play fulfillment and presentation progress before releasing followers', async () => {
    await timeline(20)
    const audio = host.querySelector('audio')!
    const video = host.querySelector('video')!
    const initialAudio = audio.currentTime
    const initialVideo = video.currentTime
    const pending = pendingPlay()
    vi.mocked(HTMLMediaElement.prototype.play).mockImplementationOnce(() => { paused.set(audio, false); return pending.promise })
    await click('播放')
    await advanceClock(100)
    expect(audio.paused).toBe(false)
    expect(video.paused).toBe(true)
    expect(host.querySelector('.preview-stage')?.getAttribute('data-preview-phase')).toBe('starting')
    // Promise fulfillment alone must not treat an unadvanced presentation
    // clock as a started decoder (the measured WebView startup condition).
    await act(async () => pending.resolve())
    await advanceClock(100)
    expect(video.paused).toBe(true)
    expect(video.currentTime).toBe(initialVideo)
    audio.currentTime = initialAudio + 0.01
    await advanceClock(10)
    expect(video.paused).toBe(false)
    expect(video.currentTime).toBe(initialVideo)
    expect(host.querySelector('.preview-stage')?.getAttribute('data-preview-phase')).toBe('playing')
  })

  it('does not release followers for progress belonging to an unresolved or superseded play promise', async () => {
    await timeline(20)
    const audio = host.querySelector('audio')!
    const video = host.querySelector('video')!
    const old = pendingPlay()
    const current = pendingPlay()
    vi.mocked(HTMLMediaElement.prototype.play).mockImplementationOnce(() => { paused.set(audio, false); return old.promise })
    await click('播放')
    audio.currentTime += 0.01
    await advanceClock(10)
    expect(video.paused).toBe(true)
    vi.mocked(HTMLMediaElement.prototype.play).mockImplementationOnce(() => { paused.set(audio, false); return current.promise })
    await act(async () => useProjectStore.getState().setPlayheadFrame(24))
    await act(async () => old.resolve())
    audio.currentTime += 0.01
    await advanceClock(10)
    expect(video.paused).toBe(true)
    await act(async () => current.resolve())
    expect(video.paused).toBe(false)
  })

  it('times out a fulfilled master play that never advances while followers stay at the prepared start', async () => {
    await timeline(20)
    const video = host.querySelector('video')!
    const position = video.currentTime
    await click('播放')
    await advanceClock(500)
    expect(host.querySelector('.preview-stage')?.getAttribute('data-preview-phase')).toBe('starting')
    expect(video.paused).toBe(true)
    expect(video.currentTime).toBe(position)
    await advanceClock(9500)
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('媒体启动超时')
    expect(host.querySelector('.preview-stage')?.getAttribute('data-playing')).toBe('false')
    expect(host.querySelector('audio')?.paused).toBe(true)
  })

  it('prepares all active decoders before starting any timeline playback', async () => {
    await timeline(20)
    const audio = host.querySelector('audio')!
    const video = host.querySelector('video')!
    Object.defineProperty(audio, 'readyState', { configurable: true, value: 1 })
    vi.mocked(HTMLMediaElement.prototype.play).mockClear()
    await click('播放')
    await act(async () => video.dispatchEvent(new Event('canplay')))
    await advanceClock(100)
    expect(video.paused).toBe(true)
    expect(audio.paused).toBe(true)
    expect(useProjectStore.getState().playheadFrame).toBe(20)
    expect(HTMLMediaElement.prototype.play).not.toHaveBeenCalled()
    Object.defineProperty(audio, 'readyState', { configurable: true, value: 4 })
    await act(async () => audio.dispatchEvent(new Event('canplay')))
    expect(audio.paused).toBe(false)
    expect(video.paused).toBe(true)
    expect(vi.mocked(HTMLMediaElement.prototype.play).mock.contexts[0]).toBe(audio)
    await advance(10)
    expect(video.paused).toBe(false)
  })

  it('keeps all nodes paused through an explicit seek and honors the newest pending target', async () => {
    await timeline(20)
    await click('播放')
    const audio = host.querySelector('audio')!
    const video = host.querySelector('video')!
    Object.defineProperty(audio, 'seeking', { configurable: true, value: true })
    await act(async () => useProjectStore.getState().setPlayheadFrame(24))
    await act(async () => video.dispatchEvent(new Event('canplay')))
    expect(video.paused).toBe(true)
    expect(audio.paused).toBe(true)
    await act(async () => useProjectStore.getState().setPlayheadFrame(28))
    Object.defineProperty(audio, 'seeking', { configurable: true, value: false })
    await act(async () => audio.dispatchEvent(new Event('seeked')))
    expect(audio.currentTime).toBeCloseTo(22 / 30)
    expect(video.currentTime).toBeCloseTo(40 / 30)
    expect(video.paused).toBe(true)
    expect(audio.paused).toBe(false)
    await advance(10)
    expect(video.paused).toBe(false)
    expect(useProjectStore.getState().playheadFrame).toBe(28)
  })

  it('waits for delayed slave metadata and seek completion without a canplay event starting it alone', async () => {
    await timeline(20)
    const video = host.querySelector('video')!
    const audio = host.querySelector('audio')!
    Object.defineProperty(video, 'readyState', { configurable: true, value: 0 })
    await click('播放')
    expect(audio.paused).toBe(true)
    expect(video.paused).toBe(true)
    Object.defineProperty(video, 'readyState', { configurable: true, value: 4 })
    Object.defineProperty(video, 'seeking', { configurable: true, value: true })
    await act(async () => video.dispatchEvent(new Event('loadedmetadata')))
    await act(async () => video.dispatchEvent(new Event('canplay')))
    expect(audio.paused).toBe(true)
    expect(video.paused).toBe(true)
    Object.defineProperty(video, 'seeking', { configurable: true, value: false })
    await act(async () => video.dispatchEvent(new Event('seeked')))
    expect(audio.paused).toBe(false)
    expect(video.paused).toBe(true)
    await advance(10)
    expect(video.paused).toBe(false)
  })

  it('revokes a pending release on pause and ignores late readiness events', async () => {
    await timeline(20)
    const video = host.querySelector('video')!
    const audio = host.querySelector('audio')!
    const pending = pendingPlay()
    vi.mocked(HTMLMediaElement.prototype.play).mockImplementationOnce(() => pending.promise)
    await click('播放')
    await click('暂停')
    await act(async () => { paused.set(audio, false); pending.resolve(); video.dispatchEvent(new Event('canplay')) })
    expect(audio.paused).toBe(true)
    expect(video.paused).toBe(true)
    await advanceClock(1000)
    expect(useProjectStore.getState().playheadFrame).toBe(20)
  })

  it('reports a preparation timeout instead of holding a never-ready decoder forever', async () => {
    await timeline(20)
    Object.defineProperty(host.querySelector('audio')!, 'readyState', { configurable: true, value: 0 })
    await click('播放')
    await advanceClock(10_000)
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('媒体准备超时')
    expect(host.querySelector('.preview-stage')?.getAttribute('data-playing')).toBe('false')
    expect(host.querySelector('audio')?.paused).toBe(true)
  })

  it('gates a discontinuous visual cut with the already playing audio master', async () => {
    const { project } = await timeline(20)
    const first = { ...project.tracks[1].clips[0], id: 'first', trackId: 'video-1', startFrame: 0, durationFrames: 30, sourceInFrame: 0 }
    const second = { ...first, id: 'second', startFrame: 30, sourceInFrame: 60 }
    project.tracks[0].clips = [first, second]; project.tracks[1].clips = []
    await act(async () => useProjectStore.setState({ project: { ...project } }))
    await click('播放')
    const audio = host.querySelector('audio')!
    const video = host.querySelector('video')!
    await advance(330)
    Object.defineProperty(video, 'readyState', { configurable: true, value: 1 })
    audio.currentTime += 0.02
    await advanceClock(20)
    expect(useProjectStore.getState().playheadFrame).toBe(30)
    expect(video.dataset.previewClipId).toBe('second')
    expect(audio.paused).toBe(true)
    expect(video.paused).toBe(true)
    Object.defineProperty(video, 'readyState', { configurable: true, value: 4 })
    await act(async () => video.dispatchEvent(new Event('canplay')))
    expect(video.currentTime).toBeCloseTo(2)
    expect(audio.paused).toBe(false)
    expect(video.paused).toBe(true)
    await advance(10)
    expect(video.paused).toBe(false)
  })
  it('freezes on a stalled audio master rather than seeking it to a wall clock', async () => {
    await timeline(20)
    await click('播放')
    const audio = host.querySelector('audio')!
    const before = audio.currentTime
    await advanceClock(1000)
    expect(useProjectStore.getState().playheadFrame).toBe(20)
    expect(audio.currentTime).toBe(before)
    expect(audio.playbackRate).toBe(1)
    expect(host.querySelector('.preview-stage')?.getAttribute('data-preview-clock')).toBe('audio')
  })

  it('holds other decoders while the master buffers and resumes with master presentation time', async () => {
    await timeline(20)
    await click('播放')
    const audio = host.querySelector('audio')!
    const video = host.querySelector('video')!
    await act(async () => audio.dispatchEvent(new Event('waiting')))
    await advanceClock(1000)
    expect(video.paused).toBe(true)
    expect(useProjectStore.getState().playheadFrame).toBe(20)
    await act(async () => { audio.currentTime += 0.2; audio.dispatchEvent(new Event('playing')) })
    await advanceClock(10)
    expect(useProjectStore.getState().playheadFrame).toBe(26)
    expect(video.paused).toBe(false)
  })

  it('detects a silent master stall, holds slaves without repeated seeks, and recovers on progress', async () => {
    await timeline(20)
    await click('播放')
    const audio = host.querySelector('audio')!
    const video = host.querySelector('video')!
    const seeks = video.dataset.previewHardSeeks
    for (let elapsed = 0; elapsed < 1000; elapsed += 10) {
      if (!video.paused) video.currentTime += 0.01 * video.playbackRate
      await advanceClock(10)
    }
    expect(audio.paused).toBe(false)
    expect(useProjectStore.getState().playheadFrame).toBe(20)
    expect(video.paused).toBe(true)
    expect(host.querySelector('.preview-stage')?.getAttribute('data-preview-buffering')).toBe('true')
    expect(video.dataset.previewHardSeeks).toBe(seeks)
    audio.currentTime += 0.1
    await advanceClock(10)
    expect(useProjectStore.getState().playheadFrame).toBe(23)
    expect(video.paused).toBe(false)
    expect(host.querySelector('.preview-stage')?.getAttribute('data-preview-buffering')).toBe('false')
  })

  it('tolerates a master time quantized to 100 ms and resets stall history on explicit seek', async () => {
    await timeline(20)
    await click('播放')
    const audio = host.querySelector('audio')!
    const video = host.querySelector('video')!
    for (let elapsed = 10; elapsed <= 200; elapsed += 10) {
      if (elapsed % 100 === 0) audio.currentTime += 0.1
      await advanceClock(10)
      expect(host.querySelector('.preview-stage')?.getAttribute('data-preview-buffering')).toBe('false')
      expect(video.paused).toBe(elapsed < 100)
    }
    await act(async () => useProjectStore.getState().setPlayheadFrame(24))
    await advance(10)
    await advanceClock(200)
    expect(host.querySelector('.preview-stage')?.getAttribute('data-preview-buffering')).toBe('false')
    expect(video.paused).toBe(false)
  })

  it('reuses a decoder for consecutive same-source clips and seeks a discontinuous cut once', async () => {
    const project = createEmptyProject()
    const first = { id: 'first', mediaId: asset.id, trackId: 'video-1', startFrame: 0, durationFrames: 30, sourceInFrame: 0, sourceDurationFrames: 120 }
    project.tracks[0].clips = [first, { ...first, id: 'second', startFrame: 30, sourceInFrame: 30 }, { ...first, id: 'jump', startFrame: 60, sourceInFrame: 0 }]
    await act(async () => useProjectStore.setState({ project, media: [asset], previewMode: 'timeline', playheadFrame: 0 }))
    const video = host.querySelector('video')!
    await click('播放')
    await advance(1000)
    expect(useProjectStore.getState().playheadFrame).toBe(30)
    expect(host.querySelector('video')).toBe(video)
    expect(video.dataset.previewClipId).toBe('second')
    expect(video.dataset.previewHardSeeks).toBe('0')
    await advance(1000)
    expect(useProjectStore.getState().playheadFrame).toBe(60)
    expect(host.querySelector('video')).toBe(video)
    expect(video.dataset.previewClipId).toBe('jump')
    expect(video.currentTime).toBeCloseTo(0)
    expect(video.dataset.previewHardSeeks).toBe('1')
  })

  it('avoids repeatedly resetting currentTime while an explicit seek is pending', async () => {
    await timeline(20)
    await click('播放')
    const audio = host.querySelector('audio')!
    Object.defineProperty(audio, 'seeking', { configurable: true, value: true })
    const before = audio.dataset.previewHardSeeks
    await act(async () => useProjectStore.getState().setPlayheadFrame(25))
    await advanceClock(500)
    await advanceClock(500)
    expect(audio.dataset.previewHardSeeks).toBe(before)
    expect(useProjectStore.getState().playheadFrame).toBe(25)
    Object.defineProperty(audio, 'seeking', { configurable: true, value: false })
    await act(async () => audio.dispatchEvent(new Event('seeked')))
    expect(audio.currentTime).toBeCloseTo(19 / 30)
    expect(Number(audio.dataset.previewHardSeeks)).toBe(Number(before) + 1)
  })

  it('uses gentle slave correction and resets rates when paused', async () => {
    await timeline(20)
    await click('播放')
    await advance(10)
    const video = host.querySelector('video')!
    const audio = host.querySelector('audio')!
    video.currentTime -= 0.1
    const seeks = video.dataset.previewHardSeeks
    await advanceClock(10)
    expect(video.playbackRate).toBeCloseTo(1.03)
    expect(video.dataset.previewHardSeeks).toBe(seeks)
    expect(audio.playbackRate).toBe(1)
    await click('暂停')
    expect(video.playbackRate).toBe(1)
    expect(audio.playbackRate).toBe(1)
  })

  it('advances an ended master at its declared boundary and fails an early source end', async () => {
    await timeline(104)
    await click('播放')
    const audio = host.querySelector('audio')!
    audio.currentTime = 99 / 30
    Object.defineProperty(audio, 'ended', { configurable: true, value: true })
    await advanceClock(10)
    expect(useProjectStore.getState().playheadFrame).toBe(105)
    expect(host.querySelector('audio')).toBeNull()
    await advanceClock(500)
    expect(useProjectStore.getState().playheadFrame).toBe(120)
    await timeline(20)
    await click('播放')
    Object.defineProperty(host.querySelector('audio')!, 'ended', { configurable: true, value: true })
    await advanceClock(10)
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('提前结束')
    expect(host.querySelector('.preview-stage')?.getAttribute('data-playing')).toBe('false')
  })

  it('ignores an obsolete play rejection after a discontinuous cut on a reused decoder', async () => {
    const project = createEmptyProject()
    const first = { id: 'first', mediaId: asset.id, trackId: 'video-1', startFrame: 0, durationFrames: 30, sourceInFrame: 0, sourceDurationFrames: 120 }
    project.tracks[0].clips = [first, { ...first, id: 'jump', startFrame: 30, sourceInFrame: 60 }]
    await act(async () => useProjectStore.setState({ project, media: [asset], previewMode: 'timeline', playheadFrame: 0 }))
    const pending = pendingPlay()
    vi.mocked(HTMLMediaElement.prototype.play).mockImplementationOnce(() => pending.promise)
    await click('播放')
    const video = host.querySelector('video')!
    await act(async () => useProjectStore.getState().setPlayheadFrame(30))
    expect(host.querySelector('video')).toBe(video)
    expect(video.currentTime).toBe(2)
    await act(async () => pending.reject(new Error('superseded play')))
    expect(host.querySelector('[role="alert"]')).toBeNull()
    expect(video.paused).toBe(false)
  })
  it('shows the playhead composition regardless of selected media or selected clip', async () => {
    await timeline(30)
    expect(host.querySelector('img')?.alt).toBe('Transparent.png')
    expect(host.querySelector('video')).toBeNull()
    expect(host.querySelector('.preview-canvas')?.getAttribute('style')).toContain('rgb(0, 0, 0)')
    await act(async () => useProjectStore.setState({ selectedMediaId: 'lower', selectedClipId: 'lower-clip', selectedClipIds: ['lower-clip'] }))
    expect(host.querySelector('img')?.alt).toBe('Transparent.png')
    expect(host.querySelector('.preview-stage')?.getAttribute('data-preview-visual')).toBe('image-clip')
  })

  it('uses rational project time for source seeks and timeline duration', async () => {
    const { project } = await timeline(60)
    await act(async () => useProjectStore.setState({ project: { ...project, timebase: { fpsNumerator: 30000, fpsDenominator: 1001 } } }))
    expect(host.querySelector('video')?.currentTime).toBeCloseTo(2.4024, 9)
    expect(host.querySelector('audio')?.currentTime).toBeCloseTo(1.8018, 9)
    expect(host.querySelector('[aria-label="时间线时间"]')?.textContent).toContain('00:00:04.004')
    expect(host.querySelector('[aria-label="时间线时间"]')?.textContent).not.toContain('00:00:02.501')
  })

  it('hides a video lane and its embedded audio while preserving independent audio', async () => {
    const { project } = await timeline(75)
    expect(host.querySelector('video')?.dataset.previewMediaId).toBe('upper')
    project.tracks[0].muted = true
    await act(async () => useProjectStore.setState({ project: { ...project } }))
    expect(host.querySelector('video')?.dataset.previewMediaId).toBe('lower')
    expect([...host.querySelectorAll('audio')].map((node) => node.dataset.previewClipId)).toEqual(['music-clip'])
    project.tracks[1].muted = true
    await act(async () => useProjectStore.setState({ project: { ...project } }))
    expect(host.querySelector('video')).toBeNull()
    expect(host.querySelector('[data-preview-gap]')).toBeTruthy()
    expect(host.querySelector('audio')?.dataset.previewClipId).toBe('music-clip')
  })

  it('does not substitute lower video when the winning upper source is unavailable', async () => {
    const { media } = await timeline(75)
    await act(async () => useProjectStore.setState({ media: media.map((item) => item.id === 'upper' ? { ...item, linkState: 'missing' } : item) }))
    expect(host.querySelector('video')).toBeNull()
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('重新链接')
    expect(host.querySelector<HTMLButtonElement>('[aria-label="播放"]')?.disabled).toBe(true)
  })

  it('clears a failed image when seeking to a different healthy timeline clip', async () => {
    await timeline(30)
    await act(async () => host.querySelector('img')!.dispatchEvent(new Event('error')))
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('图片无法显示')
    await act(async () => useProjectStore.getState().setPlayheadFrame(60))
    expect(host.querySelector('video')?.dataset.previewMediaId).toBe('lower')
    expect(host.querySelector('[role="alert"]')).toBeNull()
    expect(host.querySelector<HTMLButtonElement>('[aria-label="播放"]')?.disabled).toBe(false)
  })

  it('stops the timeline clock when the active image fails to decode', async () => {
    await timeline(30)
    await click('播放')
    await act(async () => host.querySelector('img')!.dispatchEvent(new Event('error')))
    expect(host.querySelector('.preview-stage')?.getAttribute('data-playing')).toBe('false')
    expect(host.querySelector('audio')?.paused).toBe(true)
    await advance(1000)
    expect(useProjectStore.getState().playheadFrame).toBe(30)
    expect(host.querySelector<HTMLButtonElement>('[aria-label="播放"]')?.disabled).toBe(true)
  })

  it('allows playback after seeking away from a failed video clip', async () => {
    await timeline(75)
    await act(async () => host.querySelector('video')!.dispatchEvent(new Event('error')))
    expect(host.querySelector('[role="alert"]')).toBeTruthy()
    await act(async () => useProjectStore.getState().setPlayheadFrame(90))
    expect(host.querySelector('video')?.dataset.previewMediaId).toBe('lower')
    expect(host.querySelector('[role="alert"]')).toBeNull()
    await click('播放')
    await advance(10)
    expect(host.querySelector('video')?.paused).toBe(false)
  })

  it('applies frame-based fades and pause to all active audio streams', async () => {
    const { project } = await timeline(20)
    project.tracks[3].muted = false
    await act(async () => useProjectStore.setState({ project: { ...project } }))
    const audio = [...host.querySelectorAll('audio')]
    expect(audio).toHaveLength(2)
    expect(audio[0].volume).toBeCloseTo(0.2)
    await click('播放')
    await advance(10)
    expect(audio.every((node) => !node.paused)).toBe(true)
    await click('暂停')
    expect(audio.every((node) => node.paused)).toBe(true)
    expect(host.querySelector('video')?.paused).toBe(true)
    await advance(1000)
    expect(useProjectStore.getState().playheadFrame).toBe(20)
  })

  it('keeps time moving across video, image, audio-only intervals and a silent tail', async () => {
    await timeline(0)
    await click('播放')
    const lowerBeforeImage = host.querySelector('video')!
    await advance(1000)
    expect(useProjectStore.getState().playheadFrame).toBe(30)
    expect(host.querySelector('img')).toBeTruthy()
    expect(lowerBeforeImage.paused).toBe(true)
    await advance(1000)
    expect(useProjectStore.getState().playheadFrame).toBe(60)
    expect(host.querySelector('video')?.dataset.previewMediaId).toBe('lower')
    expect(host.querySelector('video')?.currentTime).toBeCloseTo(2.4)
    await advance(1400)
    expect(useProjectStore.getState().playheadFrame).toBe(102)
    expect(host.querySelector('video')).toBeNull()
    expect(host.querySelector('[data-preview-gap]')).toBeTruthy()
    expect(host.querySelector('audio')).toBeTruthy()
    await advance(200)
    expect(useProjectStore.getState().playheadFrame).toBe(108)
    expect(host.querySelector('audio')).toBeNull()
    await advance(400)
    expect(useProjectStore.getState().playheadFrame).toBe(120)
    expect(host.querySelector('.preview-stage')?.getAttribute('data-playing')).toBe('false')
    expect(raf.size).toBe(0)
  })

  it('uses a project clock for audio-only projects and restarts from the end', async () => {
    const { project } = await timeline(0)
    project.tracks[0].clips = []; project.tracks[1].clips = []
    await act(async () => useProjectStore.setState({ project: { ...project }, playheadFrame: 120 }))
    await click('播放')
    expect(useProjectStore.getState().playheadFrame).toBe(0)
    await advance(1000)
    expect(useProjectStore.getState().playheadFrame).toBe(30)
    expect(host.querySelector('audio')?.paused).toBe(false)
    expect(host.querySelector('video')).toBeNull()
  })

  it('rebases the running clock after an explicit seek rather than snapping back', async () => {
    await timeline(0)
    await click('播放')
    await advance(500)
    await act(async () => useProjectStore.getState().setPlayheadFrame(90))
    await advance(500)
    expect(useProjectStore.getState().playheadFrame).toBe(105)
  })

  it('stops old decoders and the clock when an authoritative edit revision arrives', async () => {
    await timeline(20)
    await click('播放')
    const previousNodes = [...host.querySelectorAll<HTMLMediaElement>('video, audio')]
    await act(async () => useProjectStore.setState({ revision: 1 }))
    expect(previousNodes.every((node) => node.paused)).toBe(true)
    await advance(1000)
    expect(useProjectStore.getState().playheadFrame).toBe(20)
    expect(host.querySelector('.preview-stage')?.getAttribute('data-playing')).toBe('false')
  })

  it('supports scoped transport requests and rejects play on an empty timeline', async () => {
    await act(async () => useProjectStore.setState({ previewMode: 'timeline' }))
    const resolve = vi.fn(); const reject = vi.fn()
    await act(async () => window.dispatchEvent(new CustomEvent<TransportRequest>('kaicut:transport', { detail: { action: 'play', resolve, reject } })))
    expect(reject).toHaveBeenCalledOnce()
    expect(resolve).not.toHaveBeenCalled()
    await timeline(20)
    await act(async () => window.dispatchEvent(new CustomEvent<TransportRequest>('kaicut:transport', { detail: { action: 'play', resolve, reject } })))
    expect(resolve).toHaveBeenCalledOnce()
    expect(host.querySelector('.preview-stage')?.getAttribute('data-playing')).toBe('true')
    await act(async () => window.dispatchEvent(new CustomEvent<TransportRequest>('kaicut:transport', { detail: { action: 'pause', resolve, reject } })))
    expect(resolve).toHaveBeenCalledTimes(2)
    expect(host.querySelector('video')?.paused).toBe(true)
    expect(host.querySelector('audio')?.paused).toBe(true)
  })
})
