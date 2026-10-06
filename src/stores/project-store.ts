import { create } from 'zustand'
import { invoke, isTauri } from '@tauri-apps/api/core'
import { createEmptyProject, parseProject, serializeProject, toMediaAsset } from '@/lib/kaicut-format'
import type { MediaAsset, NativeSession, ProjectState } from '@/types/project'

export const isDesktop = isTauri()

interface ProjectActions {
  initialize: () => Promise<void>
  newProject: () => Promise<void>
  openProject: () => Promise<void>
  setProjectName: (name: string) => Promise<void>
  importMedia: (files?: FileList | File[]) => Promise<void>
  openProjectFile: (file: File) => Promise<void>
  saveProject: () => Promise<void>
  selectMedia: (id: string | null) => void
  setStatusMessage: (status: string) => void
}

function releaseUrls(media: MediaAsset[]) {
  media.forEach((asset) => { if (asset.objectUrl) URL.revokeObjectURL(asset.objectUrl) })
}

export const useProjectStore = create<ProjectState & ProjectActions>((set, get) => {
  const applySession = (session: NativeSession) => {
    releaseUrls(get().media)
    set({ project: session.project, media: session.project.mediaRefs, projectPath: session.path,
      isDirty: session.dirty, statusMessage: session.status, selectedMediaId: null })
  }
  const run = async (operation: () => Promise<void>) => {
    if (get().busy) return
    set({ busy: true })
    try { await operation() } catch (error) {
      set({ statusMessage: `操作未完成：${error instanceof Error ? error.message : String(error)}` })
    } finally { set({ busy: false }) }
  }
  const native = (command: string, args?: Record<string, unknown>) => run(async () => {
    applySession(await invoke<NativeSession>(command, args))
  })
  const mayDiscard = () => !get().isDirty || window.confirm('当前项目有未保存的修改。放弃修改并继续？')

  return {
    project: createEmptyProject(), media: [], isDirty: false, projectPath: null, busy: false,
    selectedMediaId: null, statusMessage: isDesktop ? '准备就绪' : '浏览器预览 · 桌面版支持原位保存',
    initialize: () => isDesktop ? native('project_get') : Promise.resolve(),
    newProject: () => isDesktop ? native('project_new') : run(async () => {
      if (!mayDiscard()) return
      releaseUrls(get().media)
      set({ project: createEmptyProject(), media: [], projectPath: null, isDirty: false, selectedMediaId: null, statusMessage: '已创建新项目' })
    }),
    openProject: () => native('project_open'),
    setProjectName: (name) => isDesktop ? native('project_rename', { name }) : run(async () => {
      const trimmed = name.trim()
      if (!trimmed || trimmed.length > 120) throw new Error('项目名称需为 1–120 个字符')
      if (get().project.name === trimmed) return
      set((state) => ({ project: { ...state.project, name: trimmed, updatedAt: new Date().toISOString() }, isDirty: true, statusMessage: '项目名称已修改' }))
    }),
    importMedia: (files) => isDesktop ? native('project_import') : run(async () => {
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
      set({ project, media: project.mediaRefs, projectPath: null, isDirty: false, selectedMediaId: null, statusMessage: `已打开 ${file.name} · 素材仍在原始位置` })
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
    selectMedia: (selectedMediaId) => set({ selectedMediaId }),
    setStatusMessage: (statusMessage) => set({ statusMessage }),
  }
})
