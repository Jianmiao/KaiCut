import { useEffect, useRef, useState, type CSSProperties, type KeyboardEvent as ReactKeyboardEvent, type PointerEvent as ReactPointerEvent } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { useShallow } from 'zustand/react/shallow'
import { Dialog } from '@/components/ui/dialog'
import { ExportPreflightDialog } from '@/components/editor/export-preflight-dialog'
import { Sheet } from '@/components/ui/sheet'
import { MediaBin } from '@/components/editor/media-bin'
import { Inspector } from '@/components/editor/inspector'
import { PreviewArea } from '@/components/editor/preview-area'
import { Timeline } from '@/components/editor/timeline'
import { Toolbar } from '@/components/editor/toolbar'
import { isDesktop, useProjectStore } from '@/stores/project-store'
import { usePreferences } from '@/stores/preferences-store'
import { useApplyPreferences } from '@/hooks/use-preferences'
import { useMcpBridge } from '@/hooks/use-mcp-bridge'
import type { RecoveryStatus } from '@/types/project'
import { DEFAULT_WORKSPACE, WORKSPACE_LIMITS, workspaceWidths } from '@/lib/workspace-layout'
import '@/styles/workspace-layout.css'

export default function App() {
  useMcpBridge()
  const [exportOpen, setExportOpen] = useState(false)
  const [settingsOpen, setSettingsOpen] = useState(false)
  const [recovery, setRecovery] = useState<RecoveryStatus | null>(null)
  const [showLibrary, setShowLibrary] = useState(true)
  const [showInspector, setShowInspector] = useState(true)
  const [libraryWidth, setLibraryWidth] = useState<number>(DEFAULT_WORKSPACE.library)
  const [inspectorWidth, setInspectorWidth] = useState<number>(DEFAULT_WORKSPACE.inspector)
  const [timelineHeight, setTimelineHeight] = useState<number>(DEFAULT_WORKSPACE.timeline)
  const [workspaceSize, setWorkspaceSize] = useState(1280)
  const workspaceRef = useRef<HTMLDivElement>(null)
  const widths = workspaceWidths(workspaceSize, libraryWidth, inspectorWidth, showLibrary, showInspector)
  useEffect(() => {
    const workspace = workspaceRef.current
    if (!workspace) return
    const observer = new ResizeObserver(([entry]) => { if (entry) setWorkspaceSize(entry.target.getBoundingClientRect().width) })
    observer.observe(workspace)
    return () => observer.disconnect()
  }, [])
  const [engine, setEngine] = useState('媒体引擎未加载')
  const { initialize, isDirty, statusMessage, projectPath, project, revision, checkpointRecovery, getRecoveryStatus, restoreRecovery, discardRecovery } = useProjectStore(useShallow((state) => ({
    initialize: state.initialize, isDirty: state.isDirty, statusMessage: state.statusMessage, projectPath: state.projectPath,
    project: state.project, revision: state.revision, checkpointRecovery: state.checkpointRecovery,
    getRecoveryStatus: state.getRecoveryStatus, restoreRecovery: state.restoreRecovery, discardRecovery: state.discardRecovery,
  })))
  const prefs = usePreferences()
  useApplyPreferences()
  useEffect(() => {
    let active = true
    void initialize().then(async () => {
      if (!active || !isDesktop) return
      try {
        const status = await getRecoveryStatus()
        if (active && status.available) setRecovery(status)
      } catch { /* Recovery status is optional and must not block startup. */ }
    })
    if (isDesktop) void invoke<{ message: string }>('media_engine_status').then((result) => setEngine(result.message)).catch(() => setEngine('媒体引擎检查失败'))
    return () => { active = false }
  }, [getRecoveryStatus, initialize])
  useEffect(() => {
    if (!isDesktop || !isDirty) return
    const timer = window.setTimeout(() => { void checkpointRecovery() }, 1500)
    return () => window.clearTimeout(timer)
  }, [checkpointRecovery, isDirty, project.updatedAt])
  useEffect(() => {
    if (isDesktop || !isDirty) return
    const handler = (event: BeforeUnloadEvent) => { event.preventDefault(); event.returnValue = '' }
    window.addEventListener('beforeunload', handler)
    return () => window.removeEventListener('beforeunload', handler)
  }, [isDirty])
  const startResize = (kind: 'library' | 'inspector' | 'timeline', event: ReactPointerEvent<HTMLDivElement>) => {
    event.preventDefault()
    const workspace = workspaceRef.current
    if (!workspace) return
    const rect = workspace.getBoundingClientRect()
    const move = (moveEvent: PointerEvent) => {
      if (kind === 'library') setLibraryWidth(Math.max(WORKSPACE_LIMITS.library[0], Math.min(WORKSPACE_LIMITS.library[1], workspaceSize - widths.inspector - 332, moveEvent.clientX - rect.left - 8)))
      if (kind === 'inspector') setInspectorWidth(Math.max(WORKSPACE_LIMITS.inspector[0], Math.min(WORKSPACE_LIMITS.inspector[1], workspaceSize - widths.library - 332, rect.right - moveEvent.clientX - 8)))
      if (kind === 'timeline') setTimelineHeight(Math.max(220, Math.min(520, rect.bottom - moveEvent.clientY)))
    }
    const stop = () => {
      window.removeEventListener('pointermove', move)
      window.removeEventListener('pointerup', stop)
      window.removeEventListener('pointercancel', stop)
      document.body.classList.remove(`is-resizing-workspace-${kind}`)
    }
    document.body.classList.add(`is-resizing-workspace-${kind}`)
    window.addEventListener('pointermove', move)
    window.addEventListener('pointerup', stop, { once: true })
    window.addEventListener('pointercancel', stop, { once: true })
  }
  const resetLayout = () => { setLibraryWidth(DEFAULT_WORKSPACE.library); setInspectorWidth(DEFAULT_WORKSPACE.inspector); setTimelineHeight(DEFAULT_WORKSPACE.timeline); setShowLibrary(true); setShowInspector(true) }
  const nudgeResize = (kind: 'library' | 'inspector' | 'timeline', amount: number) => {
    if (kind === 'library') setLibraryWidth(Math.max(180, Math.min(460, workspaceSize - widths.inspector - 332, widths.library + amount)))
    if (kind === 'inspector') setInspectorWidth(Math.max(220, Math.min(380, workspaceSize - widths.library - 332, widths.inspector - amount)))
    if (kind === 'timeline') setTimelineHeight((value) => Math.max(220, Math.min(520, value + amount)))
  }
  const handleResizeKeyDown = (kind: 'library' | 'inspector' | 'timeline', event: ReactKeyboardEvent<HTMLDivElement>) => {
    const amount = event.shiftKey ? 24 : 8
    if (kind === 'timeline') {
      if (event.key === 'ArrowUp') { event.preventDefault(); nudgeResize(kind, amount) }
      if (event.key === 'ArrowDown') { event.preventDefault(); nudgeResize(kind, -amount) }
      return
    }
    if (event.key === 'ArrowLeft') { event.preventDefault(); nudgeResize(kind, -amount) }
    if (event.key === 'ArrowRight') { event.preventDefault(); nudgeResize(kind, amount) }
  }
  const workspaceStyle = {
    '--library-width': `${widths.library}px`,
    '--inspector-width': `${widths.inspector}px`,
    '--timeline-height': `${timelineHeight}px`,
  } as CSSProperties
  return (
    <main className="app-shell">
      <Toolbar onExport={() => setExportOpen(true)} onSettings={() => setSettingsOpen(true)} showLibrary={showLibrary} showInspector={showInspector} onToggleLibrary={() => setShowLibrary(!showLibrary)} onToggleInspector={() => setShowInspector(!showInspector)} onResetLayout={resetLayout} />
      <div className="workspace-grid" ref={workspaceRef} style={workspaceStyle} data-library={showLibrary} data-inspector={showInspector}>
        <div className="library-slot" hidden={!showLibrary}><MediaBin /></div><PreviewArea /><div className="inspector-slot" hidden={!showInspector}><Inspector /></div><Timeline />
        {showLibrary && <div className="workspace-resizer workspace-resizer-library" role="separator" tabIndex={0} aria-label="调整素材区宽度" aria-orientation="vertical" aria-valuemin={180} aria-valuemax={Math.min(460, workspaceSize - widths.inspector - 332)} aria-valuenow={widths.library} aria-valuetext={`${widths.library} 像素`} onKeyDown={(event) => handleResizeKeyDown('library', event)} onPointerDown={(event) => startResize('library', event)} />}
        {showInspector && <div className="workspace-resizer workspace-resizer-inspector" role="separator" tabIndex={0} aria-label="调整检查器宽度" aria-orientation="vertical" aria-valuemin={220} aria-valuemax={Math.min(380, workspaceSize - widths.library - 332)} aria-valuenow={widths.inspector} aria-valuetext={`${widths.inspector} 像素`} onKeyDown={(event) => handleResizeKeyDown('inspector', event)} onPointerDown={(event) => startResize('inspector', event)} />}
        <div className="workspace-resizer workspace-resizer-timeline" role="separator" tabIndex={0} aria-label="调整时间线高度" aria-orientation="horizontal" aria-valuemin={220} aria-valuemax={520} aria-valuenow={timelineHeight} aria-valuetext={`${timelineHeight} 像素`} onKeyDown={(event) => handleResizeKeyDown('timeline', event)} onPointerDown={(event) => startResize('timeline', event)} />
      </div>
      <footer className="app-status"><span role="status" aria-live="polite">{statusMessage}</span><span title={projectPath ?? undefined}>KaiCut · 本地项目</span></footer>
      <Dialog open={Boolean(recovery)} onOpenChange={(open) => { if (!open) { setRecovery(null); void discardRecovery() } }} title="发现未恢复的编辑" description={recovery?.projectName ? `检测到“${recovery.projectName}”的自动保存快照。` : '检测到一个自动保存快照。'} confirmText="恢复项目" cancelText="删除恢复点" onConfirm={async () => { await restoreRecovery(); setRecovery(null); return true }}>
        <div className="export-placeholder"><p>恢复点只包含项目 JSON 和媒体路径，不会复制媒体文件。恢复后请显式保存项目。</p></div>
      </Dialog>
      <ExportPreflightDialog open={exportOpen} onOpenChange={setExportOpen} projectId={project.projectId} revision={revision} projectName={project.name} projectPath={projectPath} />
      <Sheet open={settingsOpen} onOpenChange={setSettingsOpen} title="外观与偏好" description="让工作台适合你的使用习惯">
        <label className="preference-row"><input type="checkbox" checked={prefs.transparency} onChange={(event) => prefs.setTransparency(event.target.checked)} /><span>启用透明效果<small>系统要求减少透明度时，始终使用实色。</small></span></label>
        <label className="preference-row"><input type="checkbox" checked={prefs.reducedMotion} onChange={(event) => prefs.setReducedMotion(event.target.checked)} /><span>减少动态效果<small>同时尊重系统设置。</small></span></label>
        <div className="engine-status"><strong>媒体引擎</strong><p>{isDesktop ? engine : '请在桌面版检查 FFmpeg'}</p><small>支持媒体信息、首帧、波形与基础播放。视频导出使用本机 H.264 编码器，设备不可用时会显示具体原因。</small></div>
      </Sheet>
    </main>
  )
}
