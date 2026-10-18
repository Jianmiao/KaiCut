import { beforeEach, describe, expect, it, vi } from 'vitest'
vi.mock('@tauri-apps/api/core', () => ({ isTauri: () => true, invoke: vi.fn() }))
import { invoke } from '@tauri-apps/api/core'
import { useProjectStore } from './project-store'
import { createEmptyProject, serializeProject } from '@/lib/kaicut-format'
import type { MediaAsset, MediaProbeResult, MediaThumbnailResult, MediaWaveformResult } from '@/types/project'

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (reason: unknown) => void
  const promise = new Promise<T>((resolvePromise, rejectPromise) => { resolve = resolvePromise; reject = rejectPromise })
  return { promise, resolve, reject }
}

function video(id: string): MediaAsset {
  return { id, name: `${id}.mp4`, kind: 'video', extension: 'mp4', size: 100, lastModified: 200, sourcePath: `E:/${id}.mp4` }
}

function readyThumbnail(mediaId: string): MediaThumbnailResult {
  return { mediaId, state: 'ready', dataUrl: `data:image/png;base64,${mediaId}` }
}

function installProject(media: MediaAsset[]) {
  const project = { ...createEmptyProject('缩略图测试'), mediaRefs: media }
  useProjectStore.setState({ project, media, selectedMediaId: null })
  return project
}

describe('native project projection', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset()
    useProjectStore.setState({ project: createEmptyProject('仍需保存'), media: [], isDirty: true, projectPath: null, busy: false, statusMessage: '', selectedMediaId: null })
  })
  it('keeps dirty state and contents after a failed save', async () => {
    vi.mocked(invoke).mockRejectedValue('磁盘空间不足')
    await useProjectStore.getState().saveProject()
    expect(useProjectStore.getState().isDirty).toBe(true)
    expect(useProjectStore.getState().project.name).toBe('仍需保存')
    expect(useProjectStore.getState().statusMessage).toContain('磁盘空间不足')
    expect(useProjectStore.getState().busy).toBe(false)
  })
  it('does not clear dirty state when a native picker is cancelled', async () => {
    vi.mocked(invoke).mockResolvedValue({ project: useProjectStore.getState().project, path: null, dirty: true, status: '已取消保存' })
    await useProjectStore.getState().saveProject()
    expect(useProjectStore.getState().isDirty).toBe(true)
  })
  it('accepts only the successful backend session as saved', async () => {
    vi.mocked(invoke).mockResolvedValue({ project: useProjectStore.getState().project, path: 'E:/我的项目.kaicut', dirty: false, status: '项目已保存' })
    await useProjectStore.getState().saveProject()
    expect(invoke).toHaveBeenCalledWith('project_save', undefined)
    expect(useProjectStore.getState().projectPath).toBe('E:/我的项目.kaicut')
    expect(useProjectStore.getState().isDirty).toBe(false)
  })
  it('preserves current project when open rejects invalid JSON', async () => {
    vi.mocked(invoke).mockRejectedValue('项目结构无效')
    await useProjectStore.getState().openProject()
    expect(useProjectStore.getState().project.name).toBe('仍需保存')
    expect(useProjectStore.getState().isDirty).toBe(true)
  })

  it('projects asynchronous media probe metadata without changing the project file state', async () => {
    const project = createEmptyProject('媒体探测')
    project.mediaRefs = [{ id: 'm1', name: '片段.mp4', kind: 'video', extension: 'mp4', size: 100, lastModified: 200, sourcePath: 'E:/片段.mp4' }]
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === 'project_import') return { project, path: null, dirty: true, status: '已导入 1 个文件' }
      if (command === 'media_check_links') return [{ id: 'm1', state: 'online' }]
      if (command === 'media_probe') return [{ mediaId: 'm1', state: 'ready', durationMs: 2000, width: 1920, height: 1080, fpsNumerator: 30, fpsDenominator: 1 }]
      if (command === 'project_recent_list') return []
      throw new Error(`unexpected command: ${command}`)
    })

    await useProjectStore.getState().importMedia()
    await vi.waitFor(() => expect(useProjectStore.getState().media[0]?.probe).toMatchObject({ state: 'ready', durationMs: 2000, width: 1920, height: 1080 }))
    expect(useProjectStore.getState().project.mediaRefs[0]).not.toHaveProperty('probe')
  })

  it('keeps runtime probe metadata when saving without changing media references', async () => {
    const project = createEmptyProject('保存时保留探测')
    const media = { id: 'm1', name: '片段.mp4', kind: 'video' as const, extension: 'mp4', size: 100, lastModified: 200, sourcePath: 'E:/片段.mp4', probe: { state: 'ready' as const, durationMs: 2000 } }
    project.mediaRefs = [{ id: media.id, name: media.name, kind: media.kind, extension: media.extension, size: media.size, lastModified: media.lastModified, sourcePath: media.sourcePath }]
    useProjectStore.setState({ project, media: [media], isDirty: true })
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === 'project_save') return { project, path: 'E:/保存.kaicut', dirty: false, status: '项目已保存' }
      if (command === 'media_check_links') return [{ id: 'm1', state: 'online' }]
      if (command === 'project_recent_list') return []
      throw new Error(`unexpected command: ${command}`)
    })

    await useProjectStore.getState().saveProject()
    expect(useProjectStore.getState().media[0]?.probe).toEqual({ state: 'ready', durationMs: 2000 })
    expect(invoke).not.toHaveBeenCalledWith('media_probe')
  })

  it('projects an ordered clip selection set and clears deleted members after authoritative batch delete', async () => {
    const project = createEmptyProject('多选测试')
    const media = video('m')
    project.mediaRefs = [media]
    project.tracks[0].clips = [
      { id: 'c1', mediaId: 'm', trackId: 'video-1', startFrame: 0, durationFrames: 30, sourceInFrame: 0, sourceDurationFrames: 90, fadeInFrames: 0, fadeOutFrames: 0 },
      { id: 'c2', mediaId: 'm', trackId: 'video-1', startFrame: 30, durationFrames: 30, sourceInFrame: 30, sourceDurationFrames: 90, fadeInFrames: 0, fadeOutFrames: 0 },
      { id: 'c3', mediaId: 'm', trackId: 'video-1', startFrame: 60, durationFrames: 30, sourceInFrame: 60, sourceDurationFrames: 90, fadeInFrames: 0, fadeOutFrames: 0 },
    ]
    useProjectStore.setState({ project, media: [media], selectedClipId: null, selectedClipIds: [], revision: 0, busy: false, canUndo: false, canRedo: false })
    useProjectStore.getState().selectClips(['c1', 'c3'], 'c3')
    expect(useProjectStore.getState().selectedClipIds).toEqual(['c1', 'c3'])
    expect(useProjectStore.getState().selectedClipId).toBe('c3')
    useProjectStore.getState().toggleClipSelection('c3')
    expect(useProjectStore.getState().selectedClipIds).toEqual(['c1'])
    useProjectStore.getState().selectClips(['c1', 'c2', 'c3'], 'c2')
    const updated = { ...project, tracks: project.tracks.map((track) => ({ ...track, clips: track.clips.filter((clip) => clip.id === 'c2') })) }
    vi.mocked(invoke).mockResolvedValue({ project: updated, path: null, dirty: true, status: '已删除 2 个片段', revision: 2, canUndo: true, canRedo: false })
    await useProjectStore.getState().editTimeline({ type: 'delete', clipIds: ['c1', 'c3'] })
    expect(invoke).toHaveBeenCalledWith('timeline_edit', { projectId: project.projectId, revision: 0, edit: { type: 'delete', clipIds: ['c1', 'c3'] } })
    expect(useProjectStore.getState().selectedClipIds).toEqual(['c2'])
    expect(useProjectStore.getState().selectedClipId).toBe('c2')
  })

  it('generates thumbnails lazily, deduplicates requests, and limits concurrency to two', async () => {
    installProject([video('a'), video('b'), video('c')])
    const first = deferred<MediaThumbnailResult>()
    const second = deferred<MediaThumbnailResult>()
    const third = deferred<MediaThumbnailResult>()
    vi.mocked(invoke).mockImplementation((command, args) => {
      expect(command).toBe('media_thumbnail')
      if (args && 'mediaId' in args && args.mediaId === 'a') return first.promise
      if (args && 'mediaId' in args && args.mediaId === 'b') return second.promise
      return third.promise
    })
    expect(invoke).not.toHaveBeenCalled()
    useProjectStore.getState().ensureThumbnail('a')
    useProjectStore.getState().ensureThumbnail('a')
    useProjectStore.getState().ensureThumbnail('b')
    useProjectStore.getState().ensureThumbnail('c')
    expect(invoke).toHaveBeenCalledTimes(2)
    expect(useProjectStore.getState().media.map((asset) => asset.thumbnail?.state)).toEqual(['loading', 'loading', 'loading'])
    first.resolve(readyThumbnail('a'))
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(3))
    useProjectStore.getState().selectMedia('c')
    second.resolve(readyThumbnail('b'))
    third.resolve(readyThumbnail('c'))
    await vi.waitFor(() => expect(useProjectStore.getState().media.every((asset) => asset.thumbnail?.state === 'ready')).toBe(true))
    expect(useProjectStore.getState().selectedMediaId).toBe('c')
    expect(useProjectStore.getState().media.find((asset) => asset.id === 'c')?.thumbnail?.dataUrl).toBe(readyThumbnail('c').dataUrl)
  })

  it('preserves selected media and thumbnails across save and rename', async () => {
    const project = installProject([video('a')])
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === 'media_thumbnail') return readyThumbnail('a')
      if (command === 'project_save' || command === 'project_rename') return { project, path: 'E:/项目.kaicut', dirty: false, status: '已保存' }
      if (command === 'media_check_links') return [{ id: 'a', state: 'online' }]
      if (command === 'project_recent_list') return []
      throw new Error(command)
    })
    useProjectStore.getState().selectMedia('a')
    useProjectStore.getState().ensureThumbnail('a')
    await vi.waitFor(() => expect(useProjectStore.getState().media[0]?.thumbnail?.state).toBe('ready'))
    await useProjectStore.getState().saveProject()
    await useProjectStore.getState().setProjectName('新名称')
    expect(useProjectStore.getState().selectedMediaId).toBe('a')
    expect(useProjectStore.getState().media[0]?.thumbnail?.dataUrl).toBe(readyThumbnail('a').dataUrl)
    expect(useProjectStore.getState().project.mediaRefs[0]).not.toHaveProperty('thumbnail')
    expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === 'media_thumbnail')).toHaveLength(1)
  })

  it('discards queued work and old results when a different project reuses the same media id', async () => {
    installProject([video('a'), video('b'), video('c')])
    const first = deferred<MediaThumbnailResult>()
    const second = deferred<MediaThumbnailResult>()
    const nextProject = { ...createEmptyProject('新项目'), mediaRefs: [video('a')] }
    let thumbnailCalls = 0
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === 'media_thumbnail') {
        thumbnailCalls += 1
        if (thumbnailCalls === 1) return first.promise
        if (thumbnailCalls === 2) return second.promise
        expect(args).toEqual({ mediaId: 'a' })
        return { ...readyThumbnail('a'), dataUrl: 'data:image/png;base64,new' }
      }
      if (command === 'project_open') return { project: nextProject, path: 'E:/新项目.kaicut', dirty: false, status: '已打开' }
      if (command === 'media_check_links') return [{ id: 'a', state: 'online' }]
      if (command === 'media_probe' || command === 'project_recent_list') return []
      throw new Error(command)
    })
    useProjectStore.getState().selectMedia('a')
    for (const id of ['a', 'b', 'c']) useProjectStore.getState().ensureThumbnail(id)
    await useProjectStore.getState().openProject()
    expect(useProjectStore.getState().selectedMediaId).toBeNull()
    expect(useProjectStore.getState().media[0]?.thumbnail).toBeUndefined()
    useProjectStore.getState().ensureThumbnail('a')
    first.resolve(readyThumbnail('a'))
    second.resolve(readyThumbnail('b'))
    await vi.waitFor(() => expect(useProjectStore.getState().media[0]?.thumbnail?.dataUrl).toBe('data:image/png;base64,new'))
    expect(thumbnailCalls).toBe(3)
    expect(invoke).not.toHaveBeenCalledWith('media_thumbnail', { mediaId: 'c' })
  })

  it('rejects stale thumbnail and probe results after relinking the same media id', async () => {
    const project = installProject([video('a')])
    const oldThumbnail = deferred<MediaThumbnailResult>()
    const oldProbe = deferred<MediaProbeResult[]>()
    const newThumbnail = deferred<MediaThumbnailResult>()
    const newProbe = deferred<MediaProbeResult[]>()
    const relinked = { ...project, mediaRefs: [{ ...video('a'), sourcePath: 'E:/替换/a.mp4', size: 999 }] }
    let thumbnailCalls = 0
    let probeCalls = 0
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === 'project_get') return { project, path: null, dirty: false, status: '已打开' }
      if (command === 'media_relink') return { project: relinked, path: null, dirty: true, status: '已链接' }
      if (command === 'media_thumbnail') return ++thumbnailCalls === 1 ? oldThumbnail.promise : newThumbnail.promise
      if (command === 'media_probe') return ++probeCalls === 1 ? oldProbe.promise : newProbe.promise
      if (command === 'media_check_links') return [{ id: 'a', state: 'online' }]
      if (command === 'project_recent_list') return []
      throw new Error(command)
    })
    await useProjectStore.getState().initialize()
    useProjectStore.getState().ensureThumbnail('a')
    await useProjectStore.getState().relinkMedia('a')
    useProjectStore.getState().ensureThumbnail('a')
    newThumbnail.resolve({ ...readyThumbnail('a'), dataUrl: 'data:image/png;base64,relinked' })
    newProbe.resolve([{ mediaId: 'a', state: 'ready', width: 3840 }])
    await vi.waitFor(() => expect(useProjectStore.getState().media[0]?.probe?.width).toBe(3840))
    oldThumbnail.resolve(readyThumbnail('a'))
    oldProbe.resolve([{ mediaId: 'a', state: 'ready', width: 640 }])
    await oldThumbnail.promise
    await oldProbe.promise
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(useProjectStore.getState().media[0]?.thumbnail?.dataUrl).toBe('data:image/png;base64,relinked')
    expect(useProjectStore.getState().media[0]?.probe?.width).toBe(3840)
  })

  it('keeps failure visible until explicitly retried and does not generate audio thumbnails', async () => {
    installProject([video('a'), { ...video('sound'), kind: 'audio', extension: 'wav', name: 'sound.wav' }])
    vi.mocked(invoke).mockRejectedValueOnce('无法解码').mockResolvedValue(readyThumbnail('a'))
    useProjectStore.getState().ensureThumbnail('sound')
    useProjectStore.getState().ensureThumbnail('a')
    await vi.waitFor(() => expect(useProjectStore.getState().media[0]?.thumbnail).toEqual({ state: 'failed', error: '无法解码' }))
    useProjectStore.getState().ensureThumbnail('a')
    expect(invoke).toHaveBeenCalledTimes(1)
    useProjectStore.getState().retryThumbnail('a')
    await vi.waitFor(() => expect(useProjectStore.getState().media[0]?.thumbnail?.state).toBe('ready'))
    expect(useProjectStore.getState().media[1]?.thumbnail?.state).toBe('unsupported')
    expect(invoke).toHaveBeenCalledTimes(2)
  })

  it('bounds cached thumbnails, preserves the selected image, and reloads evicted images on demand', async () => {
    const media = Array.from({ length: 66 }, (_, index) => video(`m${index}`))
    installProject(media)
    useProjectStore.getState().selectMedia('m0')
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      expect(command).toBe('media_thumbnail')
      if (!args || !('mediaId' in args) || typeof args.mediaId !== 'string') throw new Error('mediaId missing')
      return readyThumbnail(args.mediaId)
    })
    for (const asset of media) useProjectStore.getState().ensureThumbnail(asset.id)
    await vi.waitFor(() => expect(useProjectStore.getState().media[65]?.thumbnail?.state).toBe('ready'))
    expect(useProjectStore.getState().media.filter((asset) => asset.thumbnail?.state === 'ready')).toHaveLength(64)
    expect(useProjectStore.getState().media[0]?.thumbnail?.state).toBe('ready')
    expect(useProjectStore.getState().media[1]?.thumbnail).toBeUndefined()
    useProjectStore.getState().ensureThumbnail('m1')
    await vi.waitFor(() => expect(useProjectStore.getState().media[1]?.thumbnail?.state).toBe('ready'))
    expect(invoke).toHaveBeenCalledTimes(67)
    expect(useProjectStore.getState().media[0]?.thumbnail?.state).toBe('ready')
    expect(useProjectStore.getState().media.filter((asset) => asset.thumbnail?.state === 'ready')).toHaveLength(64)
  })

  it('applies a byte budget even when fewer than 64 large thumbnails are cached', async () => {
    const media = Array.from({ length: 5 }, (_, index) => video(`large${index}`))
    installProject(media)
    useProjectStore.getState().selectMedia('large0')
    const dataUrl = `data:image/png;base64,${'a'.repeat(4 * 1024 * 1024)}`
    vi.mocked(invoke).mockImplementation(async (_command, args) => {
      if (!args || !('mediaId' in args) || typeof args.mediaId !== 'string') throw new Error('mediaId missing')
      return { mediaId: args.mediaId, state: 'ready', dataUrl }
    })
    for (const asset of media) useProjectStore.getState().ensureThumbnail(asset.id)
    await vi.waitFor(() => expect(useProjectStore.getState().media[4]?.thumbnail?.state).toBe('ready'))
    expect(useProjectStore.getState().media[0]?.thumbnail?.state).toBe('ready')
    expect(useProjectStore.getState().media[1]?.thumbnail).toBeUndefined()
    const bytes = useProjectStore.getState().media.reduce((total, asset) => total + (asset.thumbnail?.dataUrl?.length ?? 0) * 2, 0)
    expect(bytes).toBeLessThanOrEqual(32 * 1024 * 1024)
  })
})

describe('runtime preview modes and native projection', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset()
    const project = createEmptyProject('预览模式')
    project.mediaRefs = [video('source')]
    project.tracks[0].clips = [{ id: 'clip', mediaId: 'source', trackId: 'video-1', startFrame: 10, durationFrames: 30, sourceInFrame: 5, sourceDurationFrames: 90 }]
    useProjectStore.setState({ project, media: [...project.mediaRefs], previewMode: 'timeline', selectedMediaId: null, selectedClipId: null, selectedClipIds: [], playheadFrame: 0, revision: 7, isDirty: false, busy: false, projectPath: null, canUndo: false, canRedo: false })
  })

  it('switches between source selection, timeline selection and timeline seeking without edits', () => {
    const project = useProjectStore.getState().project
    useProjectStore.getState().selectMedia('source')
    expect(useProjectStore.getState().previewMode).toBe('source')
    expect(useProjectStore.getState().selectedClipIds).toEqual([])
    useProjectStore.getState().selectClip('clip')
    expect(useProjectStore.getState().previewMode).toBe('timeline')
    expect(useProjectStore.getState().selectedClipId).toBe('clip')
    useProjectStore.getState().setPreviewMode('source')
    useProjectStore.getState().setPlayheadFrame(21.4)
    expect(useProjectStore.getState().playheadFrame).toBe(21)
    expect(useProjectStore.getState().previewMode).toBe('timeline')
    expect(useProjectStore.getState().revision).toBe(7)
    expect(useProjectStore.getState().isDirty).toBe(false)
    expect(useProjectStore.getState().project).toBe(project)
    expect(invoke).not.toHaveBeenCalled()
  })

  it('does not persist preview mode, selection or the playhead in a project file', () => {
    useProjectStore.getState().selectClip('clip')
    useProjectStore.getState().setPlayheadFrame(28)
    useProjectStore.getState().setPreviewMode('source')
    const state = useProjectStore.getState()
    const saved = JSON.parse(serializeProject(state.project, state.media))
    expect(saved).not.toHaveProperty('previewMode')
    expect(saved).not.toHaveProperty('selectedClipId')
    expect(saved).not.toHaveProperty('selectedMediaId')
    expect(saved).not.toHaveProperty('playheadFrame')
    expect(saved.tracks).toEqual(state.project.tracks.map((track) => ({ ...track, clips: track.clips.map((clip) => ({ ...clip, fadeInFrames: 0, fadeOutFrames: 0 })) })))
  })

  it('preserves valid runtime media and source view when a newer same-project native projection arrives', () => {
    const project = useProjectStore.getState().project
    const media: MediaAsset = { ...project.mediaRefs[0], linkState: 'online', probe: { state: 'ready', durationMs: 3000 }, thumbnail: { state: 'ready', dataUrl: 'data:image/png;base64,ready' }, waveform: { state: 'ready', dataUrl: 'data:image/png;base64,wave' } }
    useProjectStore.setState({ media: [media], previewMode: 'source', selectedMediaId: 'source', playheadFrame: 20 })
    useProjectStore.getState().applyNativeSession({ project: { ...project, name: '已重命名' }, path: 'E:/预览.kaicut', dirty: true, status: '已更新', revision: 8, canUndo: true, canRedo: false })
    const state = useProjectStore.getState()
    expect(state.project.name).toBe('已重命名')
    expect(state.revision).toBe(8)
    expect(state.previewMode).toBe('source')
    expect(state.selectedMediaId).toBe('source')
    expect(state.playheadFrame).toBe(20)
    expect(state.media[0].linkState).toBe('online')
    expect(state.media[0].probe).toBe(media.probe)
    expect(state.media[0].thumbnail).toBe(media.thumbnail)
    expect(state.media[0].waveform).toBe(media.waveform)
    expect(state.project.mediaRefs[0]).not.toHaveProperty('probe')
  })

  it('rejects an older projection for the same project without resetting runtime state', () => {
    const project = useProjectStore.getState().project
    useProjectStore.setState({ previewMode: 'source', selectedMediaId: 'source', playheadFrame: 21 })
    useProjectStore.getState().applyNativeSession({ project: { ...project, name: '过期名称' }, path: 'E:/旧项目.kaicut', dirty: true, status: '迟到的结果', revision: 6 })
    expect(useProjectStore.getState().project).toBe(project)
    expect(useProjectStore.getState().revision).toBe(7)
    expect(useProjectStore.getState().previewMode).toBe('source')
    expect(useProjectStore.getState().playheadFrame).toBe(21)
    expect(useProjectStore.getState().projectPath).toBeNull()
  })

  it('invalidates runtime decoder metadata after a native relink changes the source fingerprint', () => {
    const project = useProjectStore.getState().project
    useProjectStore.setState({ media: [{ ...project.mediaRefs[0], linkState: 'online', probe: { state: 'ready', durationMs: 3000 }, thumbnail: { state: 'ready', dataUrl: 'data:image/png;base64,old' } }] })
    const updated = { ...project, mediaRefs: [{ ...project.mediaRefs[0], sourcePath: 'E:/替换素材.mp4', size: 999 }] }
    useProjectStore.getState().applyNativeSession({ project: updated, path: null, dirty: true, status: '已重新链接', revision: 8 })
    expect(useProjectStore.getState().media[0].sourcePath).toBe('E:/替换素材.mp4')
    expect(useProjectStore.getState().media[0].linkState).toBe('unknown')
    expect(useProjectStore.getState().media[0].probe).toBeUndefined()
    expect(useProjectStore.getState().media[0].thumbnail).toBeUndefined()
  })

  it('resets preview and selection when the native project identity changes', () => {
    useProjectStore.getState().selectClip('clip')
    useProjectStore.getState().setPlayheadFrame(25)
    useProjectStore.getState().setPreviewMode('source')
    const project = createEmptyProject('另一个项目')
    // Rust keeps its session revision monotonic even when changing projects.
    useProjectStore.getState().applyNativeSession({ project, path: null, dirty: false, status: '已打开', revision: 8 })
    const state = useProjectStore.getState()
    expect(state.previewMode).toBe('timeline')
    expect(state.playheadFrame).toBe(0)
    expect(state.selectedClipIds).toEqual([])
    expect(state.selectedClipId).toBeNull()
    expect(state.selectedMediaId).toBeNull()
  })

  it('refreshes imported native media readiness without requiring the UI file picker', async () => {
    const before = useProjectStore.getState()
    const imported = video('imported-via-mcp')
    const project = { ...before.project, mediaRefs: [...before.project.mediaRefs, imported] }
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === 'media_check_links') return project.mediaRefs.map((media) => ({ id: media.id, state: 'online' }))
      if (command === 'media_probe') return project.mediaRefs.map((media) => ({ mediaId: media.id, state: 'ready', durationMs: 4000 }))
      throw new Error(`unexpected ${command}`)
    })
    useProjectStore.getState().applyNativeSession({ project, path: null, dirty: true, status: '已导入', revision: 8 })
    await vi.waitFor(() => expect(useProjectStore.getState().media.find((media) => media.id === imported.id)).toMatchObject({ linkState: 'online', probe: { state: 'ready', durationMs: 4000 } }))
    expect(useProjectStore.getState().isDirty).toBe(true)
    expect(useProjectStore.getState().project.mediaRefs[1]).not.toHaveProperty('probe')
  })

  it('resets an explicitly reopened same-id project and discards delayed probes from before the open', async () => {
    const project = useProjectStore.getState().project
    useProjectStore.setState({ previewMode: 'source', playheadFrame: 27, selectedClipId: 'clip', selectedClipIds: ['clip'], media: [{ ...project.mediaRefs[0], linkState: 'online', probe: { state: 'ready', durationMs: 3000 } }] })
    const old = deferred<MediaProbeResult[]>()
    let probes = 0
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === 'media_check_links') return [{ id: 'source', state: 'online' }]
      if (command === 'media_probe') return ++probes === 1 ? old.promise : [{ mediaId: 'source', state: 'ready', durationMs: 4000 }]
      throw new Error(`unexpected ${command}`)
    })
    useProjectStore.getState().applyNativeSession({ project, path: null, dirty: false, status: '已打开', revision: 8, resetEditingView: true })
    await vi.waitFor(() => expect(probes).toBe(1))
    useProjectStore.getState().applyNativeSession({ project, path: null, dirty: false, status: '再次打开', revision: 9, resetEditingView: true })
    expect(useProjectStore.getState().previewMode).toBe('timeline')
    expect(useProjectStore.getState().playheadFrame).toBe(0)
    expect(useProjectStore.getState().selectedClipIds).toEqual([])
    await vi.waitFor(() => expect(useProjectStore.getState().media[0].probe?.durationMs).toBe(4000))
    old.resolve([{ mediaId: 'source', state: 'ready', durationMs: 999 }])
    await Promise.resolve(); await Promise.resolve()
    expect(useProjectStore.getState().media[0].probe?.durationMs).toBe(4000)
  })

  it.each(['ready', 'failed'] as const)('restarts pending waveforms on same-id reopen and discards the old %s response', async (outcome) => {
    const project = createEmptyProject('波形重开')
    const audio: MediaAsset = { id: `wave-${outcome}`, name: 'audio.wav', kind: 'audio', extension: 'wav', size: 100, lastModified: 200, sourcePath: 'E:/audio.wav' }
    project.mediaRefs = [audio]
    useProjectStore.setState({ project, media: [audio], revision: 7 })
    const old = deferred<MediaWaveformResult>()
    const current = deferred<MediaWaveformResult>()
    let waveformCalls = 0
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === 'media_waveform') return ++waveformCalls === 1 ? old.promise : current.promise
      if (command === 'media_check_links') return [{ id: audio.id, state: 'online' }]
      if (command === 'media_probe') return [{ mediaId: audio.id, state: 'ready', durationMs: 5000 }]
      throw new Error(`unexpected ${command}`)
    })
    useProjectStore.getState().ensureWaveform(audio.id)
    useProjectStore.getState().applyNativeSession({ project, path: 'E:/audio.kaicut', dirty: false, status: '已重开', revision: 8, resetEditingView: true })
    useProjectStore.getState().ensureWaveform(audio.id)
    const newRequestCount = waveformCalls
    if (outcome === 'ready') old.resolve({ mediaId: audio.id, state: 'ready', dataUrl: 'data:image/png;base64,old' })
    else old.reject(new Error('old waveform failure'))
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(newRequestCount).toBe(2)
    expect(useProjectStore.getState().media[0].waveform).toEqual({ state: 'loading' })
    // The old finally must not delete the new request's deduplication token.
    useProjectStore.getState().ensureWaveform(audio.id)
    expect(waveformCalls).toBe(2)
    current.resolve({ mediaId: audio.id, state: 'ready', dataUrl: 'data:image/png;base64,new' })
    await vi.waitFor(() => expect(useProjectStore.getState().media[0].waveform).toEqual({ state: 'ready', dataUrl: 'data:image/png;base64,new', error: undefined }))
  })

  it('starts a waveform for relinked media before the previous source request finishes', async () => {
    const project = createEmptyProject('波形重新链接')
    const audio: MediaAsset = { id: 'wave-relink', name: 'audio.wav', kind: 'audio', extension: 'wav', size: 100, lastModified: 200, sourcePath: 'E:/audio.wav' }
    project.mediaRefs = [audio]
    useProjectStore.setState({ project, media: [audio], revision: 7 })
    const old = deferred<MediaWaveformResult>()
    const current = deferred<MediaWaveformResult>()
    let waveformCalls = 0
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === 'media_waveform') return ++waveformCalls === 1 ? old.promise : current.promise
      if (command === 'media_check_links') return [{ id: audio.id, state: 'online' }]
      if (command === 'media_probe') return [{ mediaId: audio.id, state: 'ready', durationMs: 5000 }]
      throw new Error(`unexpected ${command}`)
    })
    useProjectStore.getState().ensureWaveform(audio.id)
    const relinked = { ...project, mediaRefs: [{ ...audio, sourcePath: 'E:/new-audio.wav', size: 999 }] }
    useProjectStore.getState().applyNativeSession({ project: relinked, path: null, dirty: true, status: '已重新链接', revision: 8 })
    useProjectStore.getState().ensureWaveform(audio.id)
    const newRequestCount = waveformCalls
    current.resolve({ mediaId: audio.id, state: 'ready', dataUrl: 'data:image/png;base64,relinked' })
    old.resolve({ mediaId: audio.id, state: 'ready', dataUrl: 'data:image/png;base64,old-source' })
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(newRequestCount).toBe(2)
    expect(useProjectStore.getState().media[0].waveform?.dataUrl).toBe('data:image/png;base64,relinked')
  })
})
