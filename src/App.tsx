import { useEffect, useRef, useState, type CSSProperties, type KeyboardEvent as ReactKeyboardEvent, type PointerEvent as ReactPointerEvent } from 'react'
import { Download } from 'lucide-react'
import { invoke } from '@tauri-apps/api/core'
import { Dialog } from '@/components/ui/dialog'
import { Sheet } from '@/components/ui/sheet'
import { MediaBin } from '@/components/editor/media-bin'
import { Inspector } from '@/components/editor/inspector'
import { PreviewArea } from '@/components/editor/preview-area'
import { Timeline } from '@/components/editor/timeline'
import { Toolbar } from '@/components/editor/toolbar'
import { isDesktop, useProjectStore } from '@/stores/project-store'
import { usePreferences } from '@/stores/preferences-store'
import { useApplyPreferences } from '@/hooks/use-preferences'

export default function App() {
  const [exportOpen, setExportOpen] = useState(false)
  const [settingsOpen, setSettingsOpen] = useState(false)
  const [showLibrary, setShowLibrary] = useState(true)
  const [showInspector, setShowInspector] = useState(true)
  const [libraryWidth, setLibraryWidth] = useState(240)
  const [inspectorWidth, setInspectorWidth] = useState(280)
  const workspaceRef = useRef<HTMLDivElement>(null)
  const [engine, setEngine] = useState('媒体引擎未加载')
  const { initialize, isDirty, statusMessage, projectPath } = useProjectStore()
  const prefs = usePreferences()
  useApplyPreferences()
  useEffect(() => {
    void initialize()
    if (isDesktop) void invoke<{ available: boolean; version: string | null; message: string }>('media_engine_status').then((result) => setEngine(result.available ? `FFmpeg 已就绪 · ${result.version ?? ''}` : result.message)).catch(() => setEngine('媒体引擎检查失败'))
  }, [initialize])
  useEffect(() => {
    if (isDesktop || !isDirty) return
    const handler = (event: BeforeUnloadEvent) => { event.preventDefault(); event.returnValue = '' }
    window.addEventListener('beforeunload', handler)
    return () => window.removeEventListener('beforeunload', handler)
  }, [isDirty])
  const startResize = (kind: 'library' | 'inspector', event: ReactPointerEvent<HTMLDivElement>) => {
    event.preventDefault()
    const workspace = workspaceRef.current
    if (!workspace) return
    const rect = workspace.getBoundingClientRect()
    const move = (moveEvent: PointerEvent) => {
      if (kind === 'library') setLibraryWidth(Math.max(180, Math.min(360, moveEvent.clientX - rect.left)))
      if (kind === 'inspector') setInspectorWidth(Math.max(220, Math.min(380, rect.right - moveEvent.clientX)))
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
  const resetLayout = () => { setLibraryWidth(240); setInspectorWidth(280); setShowLibrary(true); setShowInspector(true) }
  const nudgeResize = (kind: 'library' | 'inspector', amount: number) => {
    if (kind === 'library') setLibraryWidth((value) => Math.max(180, Math.min(360, value + amount)))
    if (kind === 'inspector') setInspectorWidth((value) => Math.max(220, Math.min(380, value - amount)))
  }
  const handleResizeKeyDown = (kind: 'library' | 'inspector', event: ReactKeyboardEvent<HTMLDivElement>) => {
    const amount = event.shiftKey ? 24 : 8
    if (event.key === 'ArrowLeft') { event.preventDefault(); nudgeResize(kind, -amount) }
    if (event.key === 'ArrowRight') { event.preventDefault(); nudgeResize(kind, amount) }
  }
  const workspaceStyle = {
    '--library-width': `${showLibrary ? libraryWidth : 0}px`,
    '--inspector-width': `${showInspector ? inspectorWidth : 0}px`,
  } as CSSProperties
  return (
    <main className="app-shell">
      <Toolbar onExport={() => setExportOpen(true)} onSettings={() => setSettingsOpen(true)} showLibrary={showLibrary} showInspector={showInspector} onToggleLibrary={() => setShowLibrary(!showLibrary)} onToggleInspector={() => setShowInspector(!showInspector)} onResetLayout={resetLayout} />
      <div className="workspace-grid" ref={workspaceRef} style={workspaceStyle} data-library={showLibrary} data-inspector={showInspector}>
        <div className="library-slot" hidden={!showLibrary}><MediaBin /></div><PreviewArea /><div className="inspector-slot" hidden={!showInspector}><Inspector /></div><Timeline />
        {showLibrary && <div className="workspace-resizer workspace-resizer-library" role="separator" tabIndex={0} aria-label="调整素材区宽度" aria-orientation="vertical" aria-valuemin={180} aria-valuemax={360} aria-valuenow={libraryWidth} aria-valuetext={`${libraryWidth} 像素`} onKeyDown={(event) => handleResizeKeyDown('library', event)} onPointerDown={(event) => startResize('library', event)} />}
        {showInspector && <div className="workspace-resizer workspace-resizer-inspector" role="separator" tabIndex={0} aria-label="调整检查器宽度" aria-orientation="vertical" aria-valuemin={220} aria-valuemax={380} aria-valuenow={inspectorWidth} aria-valuetext={`${inspectorWidth} 像素`} onKeyDown={(event) => handleResizeKeyDown('inspector', event)} onPointerDown={(event) => startResize('inspector', event)} />}
      </div>
      <footer className="app-status"><span role="status" aria-live="polite">{statusMessage}</span><span title={projectPath ?? undefined}>KaiCut · 本地项目</span></footer>
      <Dialog open={exportOpen} onOpenChange={setExportOpen} title="导出尚未开放" description="当前阶段只提供工作台骨架。预览播放、剪辑和视频导出将在后续阶段接入。" icon={<Download size={18} />} confirmText="知道了" cancelText="关闭">
        <div className="export-placeholder"><p>你可以先导入素材并保存项目。</p></div>
      </Dialog>
      <Sheet open={settingsOpen} onOpenChange={setSettingsOpen} title="外观与偏好" description="让工作台适合你的使用习惯">
        <label className="preference-row"><input type="checkbox" checked={prefs.transparency} onChange={(event) => prefs.setTransparency(event.target.checked)} /><span>启用透明效果<small>系统要求减少透明度时，始终使用实色。</small></span></label>
        <label className="preference-row"><input type="checkbox" checked={prefs.reducedMotion} onChange={(event) => prefs.setReducedMotion(event.target.checked)} /><span>减少动态效果<small>同时尊重系统设置。</small></span></label>
        <div className="engine-status"><strong>媒体引擎</strong><p>{isDesktop ? engine : '请在桌面版检查 FFmpeg'}</p><small>本阶段未开放解码或编码。</small></div>
      </Sheet>
    </main>
  )
}
