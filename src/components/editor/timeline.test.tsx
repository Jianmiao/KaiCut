import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { invoke } from '@tauri-apps/api/core'
import { Timeline } from './timeline'
import { useProjectStore } from '@/stores/project-store'
import { createEmptyProject, maxTimelineFrames } from '@/lib/kaicut-format'

vi.mock('@tauri-apps/api/core', () => ({ isTauri: () => true, invoke: vi.fn() }))
const originalEdit = useProjectStore.getState().editTimeline
const edit = vi.fn(async () => {})
const undo = vi.fn(async () => {})
const originalUndo = useProjectStore.getState().undoTimeline
let host: HTMLDivElement
let root: Root
beforeEach(async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true })
  edit.mockClear(); undo.mockClear()
  vi.mocked(invoke).mockResolvedValue({ mediaId: 'audio', state: 'unsupported' })
  const project = createEmptyProject()
  project.mediaRefs = [{ id: 'm', name: '片段.mp4', extension: 'mp4', kind: 'video', size: 100, lastModified: 1 }]
  project.tracks[0].clips = [{ id: 'c', mediaId: 'm', trackId: 'video-1', startFrame: 30, sourceInFrame: 0, sourceDurationFrames: 120, durationFrames: 120, fadeInFrames: 0, fadeOutFrames: 0 }]
  useProjectStore.setState({ project, media: project.mediaRefs, selectedClipId: 'c', selectedClipIds: ['c'], playheadFrame: 60, revision: 1, busy: false, canUndo: true, canRedo: false, editTimeline: edit, undoTimeline: undo })
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
  await act(async () => root.render(<Timeline />))
})
afterEach(async () => {
  await act(async () => root.unmount()); host.remove()
  useProjectStore.setState({ editTimeline: originalEdit, undoTimeline: originalUndo })
})
const button = (label: string) => host.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`)!
const clipBody = (frame: number, trackId = 'video-1') => host.querySelector<HTMLButtonElement>(`[data-track-id="${trackId}"] button[aria-label*="起点 ${frame} 帧"]`)!
const pointToTrack = (trackId: string) => Object.defineProperty(document, 'elementFromPoint', { configurable: true, value: () => host.querySelector(`[data-track-id="${trackId}"]`) })
const pointer = async (target: EventTarget, type: string, options: MouseEventInit = {}) => act(async () => {
  target.dispatchEvent(new MouseEvent(type, { bubbles: true, cancelable: true, button: 0, clientX: 100, clientY: 100, ...options }))
})
const setGroup = async (differentTracks = false) => {
  const project = useProjectStore.getState().project
  const first = { ...project.tracks[0].clips[0], id: 'group-a', startFrame: 30, durationFrames: 30 }
  const second = { ...first, id: 'group-b', startFrame: 90, trackId: differentTracks ? 'video-2' : 'video-1' }
  await act(async () => useProjectStore.setState({ project: { ...project, tracks: project.tracks.map((track) => ({ ...track, clips: [first, second].filter((clip) => clip.trackId === track.id) })) }, selectedClipId: first.id, selectedClipIds: [first.id, second.id], playheadFrame: 0 }))
}
describe('timeline editing controls', () => {
  it('keeps offscreen multiselection in state without mounting every selected clip', async () => {
    const project = useProjectStore.getState().project
    const clips = Array.from({ length: 1000 }, (_, index) => ({ ...project.tracks[0].clips[0], id: `dense-${index}`, startFrame: index * 60, durationFrames: 30 }))
    const selectedClipIds = clips.map((clip) => clip.id)
    await act(async () => useProjectStore.setState({ project: { ...project, tracks: project.tracks.map((track, index) => index === 0 ? { ...track, clips } : track) }, selectedClipId: clips[0].id, selectedClipIds }))
    expect(host.querySelectorAll('.timeline-clip').length).toBeLessThan(30)
    expect(useProjectStore.getState().selectedClipIds).toEqual(selectedClipIds)
    await act(async () => button('删除所选片段（Delete） · 1000 个').click())
    expect(edit).toHaveBeenCalledWith({ type: 'delete', clipIds: selectedClipIds })
  })

  it('retains a focused clip across scrolling and releases it after focus leaves', async () => {
    const focused = clipBody(30)
    await act(async () => focused.focus())
    const scroll = host.querySelector<HTMLElement>('.timeline-scroll')!
    Object.defineProperty(scroll, 'clientWidth', { configurable: true, value: 1000 })
    await act(async () => { scroll.scrollLeft = 6400; scroll.dispatchEvent(new Event('scroll', { bubbles: true })) })
    expect(document.activeElement).toBe(focused)
    expect(host.contains(focused)).toBe(true)
    await act(async () => button('撤销（Ctrl+Z）').focus())
    expect(host.contains(focused)).toBe(false)
    expect(useProjectStore.getState().selectedClipIds).toEqual(['c'])
  })

  it('moves offscreen selected members with the visible drag anchor', async () => {
    const project = useProjectStore.getState().project
    const first = { ...project.tracks[0].clips[0], id: 'visible', durationFrames: 30 }
    const far = { ...first, id: 'offscreen', startFrame: 3000 }
    await act(async () => useProjectStore.setState({ project: { ...project, tracks: project.tracks.map((track, index) => index === 0 ? { ...track, clips: [first, far] } : track) }, selectedClipId: first.id, selectedClipIds: [first.id, far.id] }))
    expect(host.querySelectorAll('.timeline-clip')).toHaveLength(1)
    pointToTrack('video-1')
    await pointer(clipBody(30), 'pointerdown')
    await pointer(window, 'pointermove', { clientX: 164, altKey: true })
    await pointer(window, 'pointerup')
    expect(edit).toHaveBeenCalledWith({ type: 'moveMany', moves: [
      { clipId: first.id, trackId: 'video-1', startFrame: 90 },
      { clipId: far.id, trackId: 'video-1', startFrame: 3060 },
    ] })
  })

  it('shows persisted clips and sends split at the playhead', async () => {
    expect(host.querySelectorAll('.timeline-clip')).toHaveLength(1)
    await act(async () => button('分割（Ctrl+B）').click())
    expect(edit).toHaveBeenCalledWith({ type: 'split', clipId: 'c', frame: 60 })
    expect(host.querySelector('.timeline-ruler')?.getAttribute('aria-valuenow')).toBe('60')
  })
  it('disables editing at split boundaries, while busy, and on locked tracks', async () => {
    await act(async () => useProjectStore.setState({ playheadFrame: 30 }))
    expect(button('分割（Ctrl+B）').disabled).toBe(true)
    const project = useProjectStore.getState().project
    await act(async () => useProjectStore.setState({ project: { ...project, tracks: project.tracks.map((track) => ({ ...track, locked: true })) } }))
    expect(button('删除所选片段（Delete）').disabled).toBe(true)
    await act(async () => useProjectStore.setState({ busy: true }))
    expect(button('撤销（Ctrl+Z）').disabled).toBe(true)
  })
  it('protects text editing and IME input from timeline shortcuts', async () => {
    const input = document.createElement('input'); host.append(input)
    await act(async () => {
      input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Delete', bubbles: true }))
      input.dispatchEvent(new KeyboardEvent('keydown', { key: 'z', ctrlKey: true, bubbles: true }))
      host.dispatchEvent(new KeyboardEvent('keydown', { key: 'b', ctrlKey: true, isComposing: true, bubbles: true }))
    })
    expect(edit).not.toHaveBeenCalled(); expect(undo).not.toHaveBeenCalled()
    await act(async () => host.dispatchEvent(new KeyboardEvent('keydown', { key: 'b', ctrlKey: true, bubbles: true, cancelable: true })))
    expect(edit).toHaveBeenCalledWith({ type: 'split', clipId: 'c', frame: 60 })
    input.remove()
  })
  it('changes selection without editing project data and exposes track locking', async () => {
    await act(async () => { useProjectStore.setState({ selectedClipId: null, selectedClipIds: [] }); host.querySelector<HTMLButtonElement>('.timeline-clip-body')!.click() })
    expect(useProjectStore.getState().selectedClipId).toBe('c')
    expect(edit).not.toHaveBeenCalled()
    await act(async () => button('锁定视频 1').click())
    expect(edit).toHaveBeenCalledWith({ type: 'trackState', trackId: 'video-1', locked: true, muted: false })
  })
  it('supports range and additive selection, then sends one batch delete', async () => {
    const project = useProjectStore.getState().project
    project.tracks[0].clips = [
      { id: 'c1', mediaId: 'm', trackId: 'video-1', startFrame: 0, durationFrames: 60, sourceInFrame: 0, sourceDurationFrames: 180, fadeInFrames: 0, fadeOutFrames: 0 },
      { id: 'c2', mediaId: 'm', trackId: 'video-1', startFrame: 60, durationFrames: 60, sourceInFrame: 60, sourceDurationFrames: 180, fadeInFrames: 0, fadeOutFrames: 0 },
      { id: 'c3', mediaId: 'm', trackId: 'video-1', startFrame: 120, durationFrames: 60, sourceInFrame: 120, sourceDurationFrames: 180, fadeInFrames: 0, fadeOutFrames: 0 },
    ]
    await act(async () => useProjectStore.setState({ project: { ...project }, selectedClipId: null, selectedClipIds: [] }))
    const clips = () => Array.from(host.querySelectorAll<HTMLButtonElement>('.timeline-clip-body'))
    await act(async () => clips()[0].click())
    expect(useProjectStore.getState().selectedClipIds).toEqual(['c1'])
    await act(async () => clips()[2].dispatchEvent(new MouseEvent('click', { bubbles: true, shiftKey: true })))
    expect(useProjectStore.getState().selectedClipIds).toEqual(['c1', 'c2', 'c3'])
    await act(async () => clips()[1].dispatchEvent(new MouseEvent('click', { bubbles: true, ctrlKey: true })))
    expect(useProjectStore.getState().selectedClipIds).toEqual(['c1', 'c3'])
    await act(async () => button('删除所选片段（Delete） · 2 个').click())
    expect(edit).toHaveBeenLastCalledWith({ type: 'delete', clipIds: ['c1', 'c3'] })
    await act(async () => button('波纹删除（Shift+Delete） · 2 个').click())
    expect(edit).toHaveBeenLastCalledWith({ type: 'rippleDelete', clipIds: ['c1', 'c3'] })
  })
  it('shows the audio mixer, keeps unsupported gain controls honest, and uses the real mute edit', async () => {
    const project = useProjectStore.getState().project
    const audio = { id: 'audio', name: '配乐.wav', extension: 'wav', kind: 'audio' as const, size: 100, lastModified: 1 }
    const next = { ...project, mediaRefs: [...project.mediaRefs, audio], tracks: project.tracks.map((track) => track.id === 'audio-1'
      ? { ...track, clips: [{ id: 'audio-clip', mediaId: 'audio', trackId: track.id, startFrame: 0, sourceInFrame: 0, sourceDurationFrames: 120, durationFrames: 120, fadeInFrames: 0, fadeOutFrames: 0 }] }
      : track) }
    await act(async () => useProjectStore.setState({ project: next, media: next.mediaRefs, selectedClipId: 'audio-clip', selectedClipIds: ['audio-clip'], selectedMediaId: 'audio' }))
    const mixer = button('打开音频 1混音器')
    expect(mixer).toBeTruthy()
    await act(async () => mixer.click())
    expect(host.querySelector('[role="dialog"][aria-label="音频 1混音器"]')).toBeTruthy()
    expect(host.querySelector<HTMLInputElement>('[aria-label="音频 1音量"]')?.disabled).toBe(false)
    expect(host.textContent).toContain('音量')
    const volume = host.querySelector<HTMLInputElement>('[aria-label="音频 1音量"]')!
    await act(async () => { const setValue = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!; setValue.call(volume, '0.5'); volume.dispatchEvent(new Event('input', { bubbles: true })); volume.dispatchEvent(new Event('change', { bubbles: true })); volume.dispatchEvent(new PointerEvent('pointerup', { bubbles: true })) })
    expect(edit).toHaveBeenLastCalledWith({ type: 'trackVolume', trackId: 'audio-1', volume: 0.5 })
    const fadeIn = host.querySelector<HTMLInputElement>('[aria-label="音频 1淡入时长"]')!
    await act(async () => { const setValue = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!; setValue.call(fadeIn, '12'); fadeIn.dispatchEvent(new Event('input', { bubbles: true })); fadeIn.dispatchEvent(new Event('change', { bubbles: true })); fadeIn.dispatchEvent(new PointerEvent('pointerup', { bubbles: true })) })
    expect(edit).toHaveBeenLastCalledWith({ type: 'clipFade', clipId: 'audio-clip', fadeInFrames: 12, fadeOutFrames: 0 })
    await act(async () => button('停用音频 1').click())
    expect(edit).toHaveBeenLastCalledWith({ type: 'trackState', trackId: 'audio-1', locked: false, muted: true })
  })
  it('does not present fake clips when the project is empty', async () => {
    await act(async () => useProjectStore.setState({ project: createEmptyProject(), selectedClipId: null, selectedClipIds: [] }))
    expect(host.querySelector('.timeline-clip')).toBeNull()
    expect(host.textContent).toContain('双击素材')
  })
  it('commits one edit after a drag and discards a cancelled gesture', async () => {
    Object.defineProperty(document, 'elementFromPoint', { configurable: true, value: () => host.querySelector('[data-track-id="video-1"]') })
    const body = host.querySelector<HTMLButtonElement>('.timeline-clip-body')!
    await act(async () => body.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, button: 0, clientX: 100 })))
    await act(async () => window.dispatchEvent(new MouseEvent('pointermove', { clientX: 200, clientY: 200 })))
    expect(edit).not.toHaveBeenCalled()
    await act(async () => window.dispatchEvent(new MouseEvent('pointerup', { clientX: 200 })))
    expect(edit).toHaveBeenCalledTimes(1)
    expect(edit).toHaveBeenLastCalledWith({ type: 'move', clipId: 'c', trackId: 'video-1', startFrame: 124 })
    edit.mockClear()
    await act(async () => body.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, button: 0, clientX: 100 })))
    await act(async () => window.dispatchEvent(new MouseEvent('pointermove', { clientX: 160, clientY: 200 })))
    await act(async () => window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', cancelable: true })))
    await act(async () => window.dispatchEvent(new MouseEvent('pointerup', { clientX: 160 })))
    expect(edit).not.toHaveBeenCalled()
    expect(body.parentElement?.style.left).toBe('32px')
  })
  it('keeps main-rail snapping separate from ordinary snapping', async () => {
    const project = useProjectStore.getState().project
    project.tracks[0].clips = [
      { id: 'c', mediaId: 'm', trackId: 'video-1', startFrame: 30, sourceInFrame: 0, sourceDurationFrames: 300, durationFrames: 30, fadeInFrames: 0, fadeOutFrames: 0 },
      { id: 'main-boundary', mediaId: 'm', trackId: 'video-1', startFrame: 200, sourceInFrame: 30, sourceDurationFrames: 300, durationFrames: 30, fadeInFrames: 0, fadeOutFrames: 0 },
    ]
    await act(async () => useProjectStore.setState({ project: { ...project }, selectedClipId: 'c', selectedClipIds: ['c'], playheadFrame: 30, busy: false }))
    Object.defineProperty(document, 'elementFromPoint', { configurable: true, value: () => host.querySelector('[data-track-id="video-1"]') })
    await act(async () => button('吸附（拖动时按 Alt 临时关闭）').click())
    expect(button('主轨磁吸（video-1，拖到主轨片段边界）')).toBeTruthy()
    const body = host.querySelector<HTMLButtonElement>('.timeline-clip-body')!
    await act(async () => body.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, button: 0, clientX: 100 })))
    await act(async () => window.dispatchEvent(new MouseEvent('pointermove', { clientX: 280, clientY: 200 })))
    await act(async () => window.dispatchEvent(new MouseEvent('pointerup', { clientX: 280 })))
    expect(edit).toHaveBeenLastCalledWith({ type: 'move', clipId: 'c', trackId: 'video-1', startFrame: 200 })
  })
  it('moves a same-kind multi-selection once while preserving relative offsets', async () => {
    const project = useProjectStore.getState().project
    project.tracks[0].clips = [
      { id: 'group-a', mediaId: 'm', trackId: 'video-1', startFrame: 30, durationFrames: 30, sourceInFrame: 0, sourceDurationFrames: 180, fadeInFrames: 0, fadeOutFrames: 0 },
      { id: 'group-b', mediaId: 'm', trackId: 'video-1', startFrame: 90, durationFrames: 30, sourceInFrame: 30, sourceDurationFrames: 180, fadeInFrames: 0, fadeOutFrames: 0 },
    ]
    await act(async () => useProjectStore.setState({ project: { ...project }, selectedClipId: 'group-a', selectedClipIds: ['group-a', 'group-b'], playheadFrame: 0, busy: false }))
    await act(async () => { button('吸附（拖动时按 Alt 临时关闭）').click(); button('主轨磁吸（video-1，拖到主轨片段边界）').click() })
    Object.defineProperty(document, 'elementFromPoint', { configurable: true, value: () => host.querySelector('[data-track-id="video-2"]') })
    const body = Array.from(host.querySelectorAll<HTMLButtonElement>('.timeline-clip-body')).find((item) => item.getAttribute('aria-label')?.startsWith('片段.mp4，起点 30'))!
    await act(async () => body.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, button: 0, clientX: 100 })))
    await act(async () => window.dispatchEvent(new MouseEvent('pointermove', { clientX: 164, clientY: 200 })))
    await act(async () => window.dispatchEvent(new MouseEvent('pointerup', { clientX: 164 })))
    expect(edit).toHaveBeenLastCalledWith({ type: 'moveMany', moves: [
      { clipId: 'group-a', trackId: 'video-2', startFrame: 90 },
      { clipId: 'group-b', trackId: 'video-2', startFrame: 150 },
    ] })
  })
  it('keeps a same-track moving or trimmed clip visible until the gesture commits', async () => {
    pointToTrack('video-1')
    await pointer(clipBody(30), 'pointerdown')
    await pointer(window, 'pointermove', { clientX: 164, altKey: true })
    expect(host.querySelectorAll('.timeline-clip')).toHaveLength(1)
    expect(host.querySelector<HTMLElement>('.timeline-clip')!.style.left).toBe('96px')
    await pointer(window, 'pointercancel')
    await pointer(host.querySelector('.clip-trim-right')!, 'pointerdown')
    await pointer(window, 'pointermove', { clientX: 68, altKey: true })
    expect(host.querySelectorAll('.timeline-clip')).toHaveLength(1)
    expect(host.querySelector<HTMLElement>('.timeline-clip')!.style.width).toBe('96px')
    await pointer(window, 'pointerup')
    expect(edit).toHaveBeenCalledWith({ type: 'trim', clipId: 'c', startFrame: 30, sourceInFrame: 0, durationFrames: 90 })
  })
  it('preserves group selection after the click generated by a completed drag', async () => {
    await setGroup()
    pointToTrack('video-1')
    await pointer(clipBody(30), 'pointerdown')
    await pointer(window, 'pointermove', { clientX: 164, altKey: true })
    await pointer(window, 'pointerup')
    await pointer(clipBody(30), 'click')
    expect(useProjectStore.getState().selectedClipIds).toEqual(['group-a', 'group-b'])
    expect(edit).toHaveBeenCalledTimes(1)
    // A later plain click, without movement, still narrows selection.
    await pointer(clipBody(90), 'pointerdown')
    await pointer(window, 'pointerup')
    await pointer(clipBody(90), 'click')
    expect(useProjectStore.getState().selectedClipIds).toEqual(['group-b'])
  })
  it('does not start a move from an additive-selection click on an unselected clip', async () => {
    await setGroup()
    await act(async () => useProjectStore.setState({ selectedClipIds: ['group-a'], selectedClipId: 'group-a' }))
    pointToTrack('video-1')
    await pointer(clipBody(90), 'pointerdown', { ctrlKey: true })
    await pointer(window, 'pointermove', { clientX: 164, ctrlKey: true })
    await pointer(window, 'pointerup', { ctrlKey: true })
    await pointer(clipBody(90), 'click', { ctrlKey: true })
    expect(edit).not.toHaveBeenCalled()
    expect(useProjectStore.getState().selectedClipIds).toEqual(['group-a', 'group-b'])
  })
  it('preserves original track spacing when moving a selection from different tracks', async () => {
    await setGroup(true)
    pointToTrack('video-1')
    await pointer(clipBody(30), 'pointerdown')
    await pointer(window, 'pointermove', { clientX: 164, altKey: true })
    expect(host.querySelectorAll('[data-track-id="video-1"] .timeline-clip')).toHaveLength(1)
    expect(host.querySelectorAll('[data-track-id="video-2"] .timeline-clip')).toHaveLength(1)
    await pointer(window, 'pointerup')
    expect(edit).toHaveBeenCalledWith({ type: 'moveMany', moves: [
      { clipId: 'group-a', trackId: 'video-1', startFrame: 90 },
      { clipId: 'group-b', trackId: 'video-2', startFrame: 150 },
    ] })
  })
  it('supports moving vertically without requiring a horizontal pointer change', async () => {
    await setGroup()
    pointToTrack('video-2')
    await pointer(clipBody(30), 'pointerdown')
    await pointer(window, 'pointermove', { clientY: 150, altKey: true })
    await pointer(window, 'pointerup')
    expect(edit).toHaveBeenCalledWith({ type: 'moveMany', moves: [
      { clipId: 'group-a', trackId: 'video-2', startFrame: 30 },
      { clipId: 'group-b', trackId: 'video-2', startFrame: 90 },
    ] })
  })
  it('clamps the complete group at zero even when dragging its later member', async () => {
    await setGroup()
    pointToTrack('video-1')
    await pointer(clipBody(90), 'pointerdown')
    await pointer(window, 'pointermove', { clientX: -100, altKey: true })
    await pointer(window, 'pointerup')
    expect(edit).toHaveBeenCalledWith({ type: 'moveMany', moves: [
      { clipId: 'group-a', trackId: 'video-1', startFrame: 0 },
      { clipId: 'group-b', trackId: 'video-1', startFrame: 60 },
    ] })
  })
  it('clamps the complete group at the project duration limit', async () => {
    await setGroup()
    const maximum = maxTimelineFrames(useProjectStore.getState().project.timebase)
    pointToTrack('video-1')
    await pointer(clipBody(30), 'pointerdown')
    await pointer(window, 'pointermove', { clientX: 100 + maximum * 32 / 30, altKey: true })
    await pointer(window, 'pointerup')
    expect(edit).toHaveBeenCalledWith({ type: 'moveMany', moves: [
      { clipId: 'group-a', trackId: 'video-1', startFrame: maximum - 90 },
      { clipId: 'group-b', trackId: 'video-1', startFrame: maximum - 30 },
    ] })
  })
  it('does not snap a group back to another selected member', async () => {
    await setGroup()
    pointToTrack('video-1')
    await pointer(clipBody(30), 'pointerdown')
    await pointer(window, 'pointermove', { clientX: 159 })
    await pointer(window, 'pointerup')
    expect(edit).toHaveBeenCalledWith({ type: 'moveMany', moves: [
      { clipId: 'group-a', trackId: 'video-1', startFrame: 85 },
      { clipId: 'group-b', trackId: 'video-1', startFrame: 145 },
    ] })
  })
  it('does not begin a group gesture when any selected source track is locked', async () => {
    await setGroup(true)
    const project = useProjectStore.getState().project
    await act(async () => useProjectStore.setState({ project: { ...project, tracks: project.tracks.map((track) => ({ ...track, locked: track.id === 'video-2' })) } }))
    pointToTrack('video-1')
    await pointer(clipBody(30), 'pointerdown')
    await pointer(window, 'pointermove', { clientX: 164, altKey: true })
    await pointer(window, 'pointerup')
    expect(edit).not.toHaveBeenCalled()
    expect(clipBody(90, 'video-2')).toBeTruthy()
  })
  it('discards all group previews and preserves selection when Escape cancels a drag', async () => {
    await setGroup()
    pointToTrack('video-2')
    await pointer(clipBody(30), 'pointerdown')
    await pointer(window, 'pointermove', { clientX: 164, altKey: true })
    expect(host.querySelectorAll('[data-track-id="video-2"] .timeline-clip')).toHaveLength(2)
    await act(async () => window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', cancelable: true })))
    await pointer(window, 'pointerup')
    await pointer(clipBody(30), 'click')
    expect(edit).not.toHaveBeenCalled()
    expect(useProjectStore.getState().selectedClipIds).toEqual(['group-a', 'group-b'])
    expect(host.querySelectorAll('[data-track-id="video-2"] .timeline-clip')).toHaveLength(0)
    expect(clipBody(30)).toBeTruthy()
    expect(clipBody(90)).toBeTruthy()
  })
  it('does not submit a group move after the authoritative revision changes', async () => {
    await setGroup()
    pointToTrack('video-2')
    await pointer(clipBody(30), 'pointerdown')
    await pointer(window, 'pointermove', { clientX: 164, altKey: true })
    await act(async () => useProjectStore.setState({ revision: 2 }))
    await pointer(window, 'pointerup')
    expect(edit).not.toHaveBeenCalled()
    expect(host.querySelectorAll('[data-track-id="video-2"] .timeline-clip')).toHaveLength(0)
  })
})
