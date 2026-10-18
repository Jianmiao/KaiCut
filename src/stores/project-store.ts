import { create } from 'zustand'
import { invoke, isTauri } from '@tauri-apps/api/core'
import { createEmptyProject, maxTimelineFrames, parseProject, serializeProject, toMediaAsset } from '@/lib/kaicut-format'
import type { MediaAsset, MediaLinkStatus, MediaProbeResult, MediaThumbnailResult, MediaWaveformResult, NativeSession, ProjectState, RecentProject, RecoveryStatus, TimelineEdit } from '@/types/project'

export const isDesktop = isTauri()

interface ProjectActions {
  initialize: () => Promise<void>
  newProject: () => Promise<void>
  openProject: () => Promise<void>
  setProjectName: (name: string) => Promise<void>
  importMedia: (files?: FileList | File[]) => Promise<void>
  openProjectFile: (file: File) => Promise<void>
  saveProject: () => Promise<void>
  relinkMedia: (id: string) => Promise<void>
  checkpointRecovery: () => Promise<void>
  getRecoveryStatus: () => Promise<RecoveryStatus>
  restoreRecovery: () => Promise<void>
  discardRecovery: () => Promise<void>
  openRecentProject: (path: string) => Promise<void>
  loadRecentProjects: () => Promise<void>
  selectMedia: (id: string | null) => void
  selectClip: (id: string | null) => void
  selectClips: (ids: string[], primaryId?: string | null) => void
  toggleClipSelection: (id: string) => void
  setPlayheadFrame: (frame: number) => void
  setPreviewMode: (mode: 'source' | 'timeline') => void
  applyNativeSession: (session: NativeSession) => void
  editTimeline: (edit: TimelineEdit) => Promise<void>
  undoTimeline: () => Promise<void>
  redoTimeline: () => Promise<void>
  ensureThumbnail: (id: string) => void
  retryThumbnail: (id: string) => void
  ensureWaveform: (id: string) => void
  setStatusMessage: (status: string) => void
}

function releaseUrls(media: MediaAsset[]) {
  media.forEach((asset) => { if (asset.objectUrl?.startsWith('blob:')) URL.revokeObjectURL(asset.objectUrl) })
}

function sameMediaReference(left: MediaAsset, right: MediaAsset) {
  return left.id === right.id && left.name === right.name && left.kind === right.kind && left.extension === right.extension &&
    left.size === right.size && left.lastModified === right.lastModified && left.sourcePath === right.sourcePath
}

interface ThumbnailRequest {
  projectId: string
  reference: MediaAsset
}

interface WaveformRequest {
  projectId: string
  reference: MediaAsset
}

const THUMBNAIL_CACHE_LIMIT = 64
const THUMBNAIL_CACHE_BYTES = 32 * 1024 * 1024

export const useProjectStore = create<ProjectState & ProjectActions>((set, get) => {
  let probeGeneration = 0
  const thumbnailRequests = new Map<string, ThumbnailRequest>()
  let thumbnailQueue: ThumbnailRequest[] = []
  let thumbnailsInFlight = 0
  const thumbnailAccess = new Map<string, number>()
  let thumbnailClock = 0
  const waveformRequests = new Map<string, WaveformRequest>()
  const isCurrentThumbnailRequest = (request: ThumbnailRequest) => {
    const state = get()
    const asset = state.media.find((item) => item.id === request.reference.id)
    return state.project.projectId === request.projectId &&
      thumbnailRequests.get(request.reference.id) === request && !!asset && sameMediaReference(asset, request.reference)
  }
  const isCurrentWaveformRequest = (request: WaveformRequest) => {
    const state = get()
    const asset = state.media.find((item) => item.id === request.reference.id)
    return state.project.projectId === request.projectId &&
      waveformRequests.get(request.reference.id) === request && !!asset && sameMediaReference(asset, request.reference)
  }
  const trimThumbnails = () => {
    const state = get()
    const cached = state.media.filter((asset) => asset.thumbnail?.state === 'ready' && asset.thumbnail.dataUrl)
    let count = cached.length
    // JS engines may store strings as UTF-16. Account for both bytes per character.
    let bytes = cached.reduce((total, asset) => total + asset.thumbnail!.dataUrl!.length * 2, 0)
    const evicted = new Set<string>()
    const candidates = cached.filter((asset) => asset.id !== state.selectedMediaId)
      .sort((left, right) => (thumbnailAccess.get(left.id) ?? 0) - (thumbnailAccess.get(right.id) ?? 0))
    for (const asset of candidates) {
      if (count <= THUMBNAIL_CACHE_LIMIT && bytes <= THUMBNAIL_CACHE_BYTES) break
      evicted.add(asset.id)
      thumbnailAccess.delete(asset.id)
      count -= 1
      bytes -= asset.thumbnail!.dataUrl!.length * 2
    }
    if (evicted.size) set((current) => ({ media: current.media.map((asset) => evicted.has(asset.id) ? { ...asset, thumbnail: undefined } : asset) }))
  }
  const pumpThumbnails = () => {
    while (thumbnailsInFlight < 2 && thumbnailQueue.length > 0) {
      const request = thumbnailQueue.shift()!
      if (!isCurrentThumbnailRequest(request)) continue
      thumbnailsInFlight += 1
      void invoke<MediaThumbnailResult>('media_thumbnail', { mediaId: request.reference.id }).then((result) => {
        if (!isCurrentThumbnailRequest(request)) return
        if (result.mediaId !== request.reference.id || (result.state === 'ready' && !result.dataUrl?.startsWith('data:image/png;base64,'))) {
          throw new Error('缩略图结果无效')
        }
        thumbnailAccess.set(request.reference.id, ++thumbnailClock)
        set((state) => ({ media: state.media.map((asset) => asset.id === result.mediaId
          ? { ...asset, thumbnail: { state: result.state, dataUrl: result.state === 'ready' ? result.dataUrl : undefined, error: result.error } }
          : asset) }))
        trimThumbnails()
      }).catch((error: unknown) => {
        if (!isCurrentThumbnailRequest(request)) return
        set((state) => ({ media: state.media.map((asset) => asset.id === request.reference.id
          ? { ...asset, thumbnail: { state: 'failed', error: error instanceof Error ? error.message : String(error) } }
          : asset) }))
      }).finally(() => {
        thumbnailsInFlight -= 1
        if (thumbnailRequests.get(request.reference.id) === request) thumbnailRequests.delete(request.reference.id)
        pumpThumbnails()
      })
    }
  }
  const ensureThumbnail = (id: string) => {
    const state = get()
    const asset = state.media.find((item) => item.id === id)
    if (!asset) return
    if (asset.thumbnail?.state === 'ready') {
      thumbnailAccess.set(id, ++thumbnailClock)
      return
    }
    const pending = thumbnailRequests.get(id)
    if (pending && isCurrentThumbnailRequest(pending)) {
      const queued = thumbnailQueue.indexOf(pending)
      if (id === state.selectedMediaId && queued > 0) {
        thumbnailQueue.splice(queued, 1)
        thumbnailQueue.unshift(pending)
      }
      return
    }
    if (asset.thumbnail && asset.thumbnail.state !== 'loading') return
    if (!isDesktop || asset.kind === 'audio') {
      set((current) => ({ media: current.media.map((item) => item.id === id ? { ...item, thumbnail: { state: 'unsupported' } } : item) }))
      return
    }
    const request = { projectId: state.project.projectId, reference: asset }
    thumbnailRequests.set(id, request)
    if (id === state.selectedMediaId) thumbnailQueue.unshift(request)
    else thumbnailQueue.push(request)
    set((current) => ({ media: current.media.map((item) => item.id === id ? { ...item, thumbnail: { state: 'loading' } } : item) }))
    pumpThumbnails()
  }
  const ensureWaveform = (id: string) => {
    const state = get()
    const asset = state.media.find((item) => item.id === id)
    if (!asset || asset.kind !== 'audio') return
    const pending = waveformRequests.get(id)
    if (asset.waveform?.state === 'ready' || (pending && isCurrentWaveformRequest(pending))) return
    if (!isDesktop) {
      set((current) => ({ media: current.media.map((item) => item.id === id ? { ...item, waveform: { state: 'unsupported' } } : item) }))
      return
    }
    const request = { projectId: state.project.projectId, reference: asset }
    waveformRequests.set(id, request)
    set((current) => ({ media: current.media.map((item) => item.id === id ? { ...item, waveform: { state: 'loading' } } : item) }))
    void invoke<MediaWaveformResult>('media_waveform', { mediaId: id }).then((result) => {
      if (!isCurrentWaveformRequest(request)) return
      set((state) => ({ media: state.media.map((item) => item.id === id ? { ...item, waveform: { state: result.state, dataUrl: result.dataUrl, error: result.error } } : item) }))
    }).catch((error: unknown) => {
      if (!isCurrentWaveformRequest(request)) return
      set((state) => ({ media: state.media.map((item) => item.id === id ? { ...item, waveform: { state: 'failed', error: error instanceof Error ? error.message : String(error) } } : item) }))
    }).finally(() => { if (waveformRequests.get(id) === request) waveformRequests.delete(id) })
  }
  const applySession = (session: NativeSession, resetEditingView = false) => {
    const previousState = get()
    if (session.revision !== undefined && session.revision < previousState.revision) return
    const sameProject = previousState.project.projectId === session.project.projectId
    const reloaded = resetEditingView && session.revision !== previousState.revision
    const preserveEditingView = sameProject && !reloaded
    const previousMedia = previousState.media
    const previousById = new Map(previousMedia.map((asset) => [asset.id, asset]))
    const nextMedia = session.project.mediaRefs.map((asset) => {
      const previous = previousById.get(asset.id)
      const unchanged = sameProject && !reloaded && previous && sameMediaReference(previous, asset)
      return {
        ...asset,
        linkState: unchanged ? previous.linkState ?? 'unknown' : 'unknown',
        linkReason: unchanged ? previous.linkReason : undefined,
        probe: unchanged ? previous.probe : undefined,
        thumbnail: unchanged ? previous.thumbnail : undefined,
        waveform: unchanged ? previous.waveform : undefined,
      }
    })
    const mediaChanged = previousMedia.length !== nextMedia.length || nextMedia.some((asset) => {
      const previous = previousById.get(asset.id)
      return !previous || !sameMediaReference(previous, asset)
    })
    if (!sameProject || reloaded || mediaChanged) probeGeneration += 1
    const nextById = new Map(nextMedia.map((asset) => [asset.id, asset]))
    for (const [id, request] of thumbnailRequests) {
      const next = nextById.get(id)
      if (!sameProject || reloaded || !next || !sameMediaReference(request.reference, next)) thumbnailRequests.delete(id)
    }
    for (const [id, request] of waveformRequests) {
      const next = nextById.get(id)
      if (!sameProject || reloaded || !next || !sameMediaReference(request.reference, next)) waveformRequests.delete(id)
    }
    thumbnailQueue = thumbnailQueue.filter((request) => thumbnailRequests.get(request.reference.id) === request)
    for (const id of thumbnailAccess.keys()) {
      if (!sameProject || !nextById.get(id)?.thumbnail) thumbnailAccess.delete(id)
    }
    releaseUrls(previousMedia)
    const validClipIds = new Set(session.project.tracks.flatMap((track) => track.clips.map((clip) => clip.id)))
    const priorSelection = previousState.selectedClipIds ?? (previousState.selectedClipId ? [previousState.selectedClipId] : [])
    const selectedClipIds = preserveEditingView ? priorSelection.filter((id) => validClipIds.has(id)) : []
    const selectedClipId = preserveEditingView && selectedClipIds.includes(previousState.selectedClipId ?? '')
      ? previousState.selectedClipId : selectedClipIds.at(-1) ?? null
    set({ project: session.project, media: nextMedia, projectPath: session.path,
      isDirty: session.dirty, statusMessage: session.status,
      revision: session.revision ?? (sameProject ? previousState.revision : 0),
      canUndo: session.canUndo ?? false, canRedo: session.canRedo ?? false,
      selectedClipId, selectedClipIds, previewMode: preserveEditingView ? previousState.previewMode : 'timeline', playheadFrame: preserveEditingView ? previousState.playheadFrame : 0,
      selectedMediaId: preserveEditingView && nextMedia.some((asset) => asset.id === previousState.selectedMediaId) ? previousState.selectedMediaId : null })
  }
  const refreshMediaLinks = async () => {
    if (!isDesktop || get().media.length === 0) return
    const projectId = get().project.projectId
    const references = new Map(get().media.map((asset) => [asset.id, asset]))
    const generation = probeGeneration
    try {
      const statuses = await invoke<MediaLinkStatus[]>('media_check_links')
      if (get().project.projectId !== projectId || generation !== probeGeneration) return
      const byId = new Map(statuses.map((status) => [status.id, status]))
      set((state) => ({ media: state.media.map((asset) => {
        const status = byId.get(asset.id)
        const reference = references.get(asset.id)
        return status && reference && sameMediaReference(reference, asset) ? { ...asset, linkState: status.state, linkReason: status.reason } : asset
      }) }))
    } catch {
      // A link check must not turn a successful project operation into an error.
    }
  }
  const refreshMediaProbes = async () => {
    if (!isDesktop || get().media.length === 0) return
    const generation = ++probeGeneration
    const projectId = get().project.projectId
    const mediaIds = get().media.map((asset) => asset.id)
    set((state) => ({ media: state.media.map((asset) => mediaIds.includes(asset.id) ? { ...asset, probe: { state: 'probing' } } : asset) }))
    try {
      const results = await invoke<MediaProbeResult[]>('media_probe')
      if (generation !== probeGeneration || get().project.projectId !== projectId) return
      const byId = new Map(results.map((result) => [result.mediaId, result]))
      set((state) => ({ media: state.media.map((asset) => {
        const result = byId.get(asset.id)
        if (!result) return asset
        return { ...asset, probe: {
          state: result.state,
          durationMs: result.durationMs,
          width: result.width,
          height: result.height,
          fpsNumerator: result.fpsNumerator,
          fpsDenominator: result.fpsDenominator,
          audioSampleRate: result.audioSampleRate,
          audioChannels: result.audioChannels,
          rotation: result.rotation,
          error: result.error,
        } }
      }) }))
    } catch (error) {
      if (generation !== probeGeneration || get().project.projectId !== projectId) return
      const message = error instanceof Error ? error.message : String(error)
      set((state) => ({ media: state.media.map((asset) => mediaIds.includes(asset.id) ? { ...asset, probe: { state: 'failed', error: message } } : asset) }))
    }
  }
  const run = async (operation: () => Promise<void>) => {
    if (get().busy) return
    set({ busy: true })
    try { await operation() } catch (error) {
      set({ statusMessage: `操作未完成：${error instanceof Error ? error.message : String(error)}` })
    } finally { set({ busy: false }) }
  }
  const native = (command: string, args?: Record<string, unknown>, probeMedia = false) => run(async () => {
    applySession(await invoke<NativeSession>(command, args), ['project_new', 'project_open', 'project_open_recent', 'project_recovery_restore'].includes(command))
    await refreshMediaLinks()
    if (probeMedia) void refreshMediaProbes()
    await refreshRecentProjects()
  })
  const refreshRecentProjects = async () => {
    if (!isDesktop) return
    try { set({ recentProjects: await invoke<RecentProject[]>('project_recent_list') }) } catch { /* Recent projects are optional. */ }
  }
  const mayDiscard = () => !get().isDirty || window.confirm('当前项目有未保存的修改。放弃修改并继续？')
  const selectClips = (ids: string[], primaryId?: string | null) => {
    const clips = get().project.tracks.flatMap((track) => track.clips)
    const valid = new Set(clips.map((clip) => clip.id))
    const selectedClipIds = [...new Set(ids)].filter((id) => valid.has(id))
    const selectedClipId = primaryId && selectedClipIds.includes(primaryId) ? primaryId : selectedClipIds.at(-1) ?? null
    const clip = clips.find((item) => item.id === selectedClipId)
    set({ selectedClipIds, selectedClipId, previewMode: 'timeline', ...(clip ? { selectedMediaId: clip.mediaId } : selectedClipIds.length === 0 ? { selectedMediaId: null } : {}) })
    if (clip) thumbnailAccess.set(clip.mediaId, ++thumbnailClock)
  }
  const selectClip = (id: string | null) => selectClips(id ? [id] : [])
  const toggleClipSelection = (id: string) => {
    const state = get()
    const current = state.selectedClipIds ?? (state.selectedClipId ? [state.selectedClipId] : [])
    const next = current.includes(id)
      ? current.filter((item) => item !== id)
      : [...current, id]
    selectClips(next, next.includes(id) ? id : next.at(-1) ?? null)
  }
  const timelineCommand = (command: 'timeline_edit' | 'timeline_undo' | 'timeline_redo', edit?: TimelineEdit) => run(async () => {
    if (!isDesktop) throw new Error('请在桌面版剪辑')
    const before = get()
    const projectId = before.project.projectId
    const revision = before.revision
    const oldClipIds = new Set(before.project.tracks.flatMap((track) => track.clips.map((clip) => clip.id)))
    const session = await invoke<NativeSession>(command, { projectId, revision, ...(edit ? { edit } : {}) })
    // Never overwrite a newer project projection with a delayed edit response.
    if (get().project.projectId !== projectId || get().revision !== revision) return
    if (session.project.projectId !== projectId) throw new Error('剪辑结果与当前项目不匹配')
    applySession(session)
    const clips = session.project.tracks.flatMap((track) => track.clips)
    if (edit?.type === 'add' || edit?.type === 'duplicate' || edit?.type === 'split') {
      const added = clips.find((clip) => !oldClipIds.has(clip.id))
      if (added) selectClip(added.id)
    } else if (edit?.type === 'delete' || edit?.type === 'rippleDelete') {
      const priorSelection = before.selectedClipIds ?? (before.selectedClipId ? [before.selectedClipId] : [])
      const remaining = priorSelection.filter((id) => !edit.clipIds.includes(id))
      selectClips(remaining, remaining.at(-1) ?? null)
    }
  })

  return {
    project: createEmptyProject(), media: [], recentProjects: [], isDirty: false, projectPath: null, busy: false,
    selectedClipId: null, selectedClipIds: [], previewMode: 'timeline', playheadFrame: 0, revision: 0, canUndo: false, canRedo: false,
    selectedMediaId: null, statusMessage: isDesktop ? '准备就绪' : '浏览器预览 · 桌面版支持原位保存',
    initialize: () => isDesktop ? native('project_get', undefined, true) : Promise.resolve(),
    newProject: () => isDesktop ? native('project_new') : run(async () => {
      if (!mayDiscard()) return
      releaseUrls(get().media)
      set({ project: createEmptyProject(), media: [], projectPath: null, isDirty: false, selectedMediaId: null, selectedClipId: null, selectedClipIds: [], previewMode: 'timeline', playheadFrame: 0, revision: 0, canUndo: false, canRedo: false, statusMessage: '已创建新项目' })
    }),
    openProject: () => native('project_open', undefined, true),
    setProjectName: (name) => isDesktop ? native('project_rename', { name }) : run(async () => {
      const trimmed = name.trim()
      if (!trimmed || trimmed.length > 120) throw new Error('项目名称需为 1–120 个字符')
      if (get().project.name === trimmed) return
      set((state) => ({ project: { ...state.project, name: trimmed, updatedAt: new Date().toISOString() }, isDirty: true, statusMessage: '项目名称已修改' }))
    }),
    importMedia: (files) => isDesktop ? native('project_import', undefined, true) : run(async () => {
      if (!files) return
      const accepted: MediaAsset[] = []
      let rejected = 0
      for (const file of Array.from(files)) {
        const duplicate = [...get().media, ...accepted].some((asset) => asset.name === file.name && asset.size === file.size && asset.lastModified === file.lastModified)
        if (duplicate) { rejected++; continue }
        try { accepted.push(toMediaAsset(file)) } catch { rejected++ }
      }
      set((state) => ({ media: [...state.media, ...accepted], isDirty: state.isDirty || accepted.length > 0,
        project: { ...state.project, updatedAt: new Date().toISOString() },
        statusMessage: `已导入 ${accepted.length} 个文件${rejected ? `，跳过 ${rejected} 个重复或不支持文件` : ''}` }))
    }),
    openProjectFile: (file) => run(async () => {
      if (isDesktop) throw new Error('请使用桌面文件选择器打开项目')
      if (file.size > 10 * 1024 * 1024) throw new Error('项目文件超过 10 MiB')
      if (!file.name.toLowerCase().endsWith('.kaicut')) throw new Error('请选择 .kaicut 文件')
      const project = parseProject(await file.text())
      if (!mayDiscard()) return
      releaseUrls(get().media)
      set({ project, media: project.mediaRefs, projectPath: null, isDirty: false, selectedMediaId: null, selectedClipId: null, selectedClipIds: [], previewMode: 'timeline', playheadFrame: 0, revision: 0, canUndo: false, canRedo: false, statusMessage: `已打开 ${file.name} · 素材仍在原始位置` })
    }),
    saveProject: () => isDesktop ? native('project_save') : run(async () => {
      const { project, media } = get()
      const url = URL.createObjectURL(new Blob([serializeProject(project, media)], { type: 'application/json' }))
      const anchor = document.createElement('a')
      anchor.href = url
      anchor.download = `${project.name.replace(/[<>:"/\\|?*]/g, '_') || '未命名项目'}.kaicut`
      document.body.appendChild(anchor)
      anchor.click()
      anchor.remove()
      setTimeout(() => URL.revokeObjectURL(url), 60_000)
      // A browser cannot confirm a completed write; keep dirty until a desktop save.
      set({ statusMessage: '已请求下载项目副本；请确认下载完成。桌面版支持原位保存。' })
    }),
    relinkMedia: (id) => isDesktop ? native('media_relink', { mediaId: id }, true) : Promise.resolve(),
    checkpointRecovery: async () => {
      if (!isDesktop || !get().isDirty) return
      try { await invoke('project_recovery_checkpoint') } catch { /* Recovery must not interrupt editing. */ }
    },
    getRecoveryStatus: async () => isDesktop ? invoke<RecoveryStatus>('project_recovery_status') : ({ available: false }),
    restoreRecovery: async () => {
      if (!isDesktop) return
      await native('project_recovery_restore', undefined, true)
    },
    discardRecovery: async () => {
      if (!isDesktop) return
      try { await invoke('project_recovery_discard') } catch { /* An absent snapshot is already discarded. */ }
    },
    openRecentProject: (path) => isDesktop ? native('project_open_recent', { path }, true) : Promise.resolve(),
    loadRecentProjects: refreshRecentProjects,
    selectMedia: (selectedMediaId) => {
      set({ selectedMediaId, selectedClipId: null, selectedClipIds: [], previewMode: 'source' })
      if (selectedMediaId) thumbnailAccess.set(selectedMediaId, ++thumbnailClock)
    },
    selectClip,
    selectClips,
    toggleClipSelection,
    setPlayheadFrame: (frame) => {
      if (!Number.isFinite(frame)) return
      set({ playheadFrame: Math.max(0, Math.min(maxTimelineFrames(get().project.timebase), Math.round(frame))), previewMode: 'timeline' })
    },
    setPreviewMode: (previewMode) => set({ previewMode }),
    applyNativeSession: (session) => {
      // Rust revisions advance across new/open as well as edits; a delayed MCP
      // event must never bring a project back after the user switched away.
      if ((session.revision ?? 0) < get().revision) return
      const before = get()
      const mediaChanged = before.project.projectId !== session.project.projectId || session.resetEditingView
        || before.media.length !== session.project.mediaRefs.length
        || session.project.mediaRefs.some((asset) => {
          const previous = before.media.find((item) => item.id === asset.id)
          return !previous || !sameMediaReference(previous, asset)
        })
      applySession(session, session.resetEditingView)
      if (mediaChanged) {
        const generation = probeGeneration
        // Probing advances the shared generation; starting it before links
        // return would discard those results and leave new media unknown.
        void refreshMediaLinks().then(() => {
          if (get().project.projectId === session.project.projectId && probeGeneration === generation) void refreshMediaProbes()
        })
      }
    },
    editTimeline: (edit) => timelineCommand('timeline_edit', edit),
    undoTimeline: () => timelineCommand('timeline_undo'),
    redoTimeline: () => timelineCommand('timeline_redo'),
    ensureThumbnail,
    ensureWaveform,
    retryThumbnail: (id) => {
      const pending = thumbnailRequests.get(id)
      if (pending && isCurrentThumbnailRequest(pending)) return
      set((state) => ({ media: state.media.map((asset) => asset.id === id ? { ...asset, thumbnail: undefined } : asset) }))
      ensureThumbnail(id)
    },
    setStatusMessage: (statusMessage) => set({ statusMessage }),
  }
})
