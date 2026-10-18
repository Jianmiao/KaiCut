import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { Inspector } from './inspector'
import { useProjectStore } from '@/stores/project-store'
import { createEmptyProject } from '@/lib/kaicut-format'
import type { MediaAsset } from '@/types/project'

vi.mock('@tauri-apps/api/core', () => ({ isTauri: () => true, invoke: vi.fn() }))
const originalEdit = useProjectStore.getState().editTimeline
const edit = vi.fn(async () => {})
let host: HTMLDivElement
let root: Root
const media: MediaAsset = { id: 'm', name: '图片.png', kind: 'image', extension: 'png', size: 100, lastModified: 1, linkState: 'online', probe: { state: 'ready', width: 100, height: 100 } }
beforeEach(async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true }); edit.mockClear()
  const project = createEmptyProject()
  project.mediaRefs = [{ id: media.id, name: media.name, kind: media.kind, extension: media.extension, size: media.size, lastModified: media.lastModified }]
  useProjectStore.setState({ project, media: [media], selectedMediaId: media.id, selectedClipId: null, busy: false, statusMessage: '', editTimeline: edit })
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
  await act(async () => root.render(<Inspector />))
})
afterEach(async () => { await act(async () => root.unmount()); host.remove(); useProjectStore.setState({ editTimeline: originalEdit }) })
describe('inspector timeline editing', () => {
  it('adds an image without fabricated duration, using the first unlocked video track', async () => {
    const project = useProjectStore.getState().project
    await act(async () => useProjectStore.setState({ project: { ...project, tracks: project.tracks.map((t) => t.id === 'video-1' ? { ...t, locked: true } : t) } }))
    const add = Array.from(host.querySelectorAll('button')).find((button) => button.textContent?.includes('添加到时间线'))!
    await act(async () => add.click())
    expect(edit).toHaveBeenCalledWith({ type: 'add', mediaId: 'm', trackId: 'video-2' })
    expect(host.textContent).toContain('5 秒')
  })
  it('blocks missing or unprobed media and keeps failures visible', async () => {
    await act(async () => useProjectStore.setState({ media: [{ ...media, linkState: 'missing' }] }))
    const add = Array.from(host.querySelectorAll('button')).find((button) => button.textContent?.includes('添加到时间线'))!
    expect(add.disabled).toBe(true)
    await act(async () => useProjectStore.setState({ media: [{ ...media, probe: { state: 'failed', error: '不可识别' } }] }))
    expect(add.disabled).toBe(true)
    expect(host.textContent).toContain('不可识别')
  })
  it('exposes frame forms and sends a single trim command for the selected clip', async () => {
    const project = useProjectStore.getState().project
    project.tracks[0].clips = [{ id: 'c', mediaId: 'm', trackId: 'video-1', startFrame: 0, durationFrames: 150, sourceInFrame: 0, sourceDurationFrames: 2592000 }]
    await act(async () => useProjectStore.setState({ project: { ...project }, selectedClipId: 'c' }))
    expect(host.textContent).toContain('片段属性')
    const submit = Array.from(host.querySelectorAll('button')).find((button) => button.textContent?.includes('应用裁切'))!
    await act(async () => submit.click())
    expect(edit).toHaveBeenCalledWith({ type: 'trim', clipId: 'c', sourceInFrame: 0, startFrame: 0, durationFrames: 150 })
  })
})
