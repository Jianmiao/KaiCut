import { useEffect, useRef, useState, type MouseEvent as ReactMouseEvent } from 'react'
import { Check, CheckCircle2, ChevronDown, Clock3, Copy, FolderOpen, Keyboard, Minus, PanelsTopLeft, Plus, Save, Settings2, Square, Upload, X } from 'lucide-react'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { useShallow } from 'zustand/react/shallow'
import { Button } from '@/components/ui/button'
import { DropdownMenu } from '@/components/ui/dropdown-menu'
import { Input } from '@/components/ui/input'
import { Dialog } from '@/components/ui/dialog'
import { isDesktop, useProjectStore } from '@/stores/project-store'
import '@/styles/toolbar.css'

interface Props { onExport: () => void; onSettings: () => void; showLibrary: boolean; showInspector: boolean; onToggleLibrary: () => void; onToggleInspector: () => void; onResetLayout: () => void }
export function Toolbar({ onExport, onSettings, showLibrary, showInspector, onToggleLibrary, onToggleInspector, onResetLayout }: Props) {
  const picker = useRef<HTMLInputElement>(null)
  const { project, projectPath, isDirty, busy, recentProjects = [], newProject, openProject, openRecentProject, openProjectFile, saveProject, setProjectName, setStatusMessage } = useProjectStore(useShallow((state) => ({
    project: state.project, projectPath: state.projectPath, isDirty: state.isDirty, busy: state.busy, recentProjects: state.recentProjects,
    newProject: state.newProject, openProject: state.openProject, openRecentProject: state.openRecentProject,
    openProjectFile: state.openProjectFile, saveProject: state.saveProject, setProjectName: state.setProjectName, setStatusMessage: state.setStatusMessage,
  })))
  const [draftName, setDraftName] = useState('')
  const [renameOpen, setRenameOpen] = useState(false)
  const [shortcutsOpen, setShortcutsOpen] = useState(false)
  const [maximized, setMaximized] = useState(false)
  const mac = /Mac/.test(navigator.platform)
  const windows = isDesktop && /Win/.test(navigator.platform)
  const mod = mac ? '⌘' : 'Ctrl+'
  const open = () => { if (isDesktop) void openProject(); else picker.current?.click() }
  const saveStatus = busy ? '处理中…' : isDirty ? '未保存' : projectPath ? '已保存' : '新项目'
  const shortcuts = [
    { label: '新建项目', key: `${mod}N` },
    { label: '打开项目', key: `${mod}O` },
    { label: '保存项目', key: `${mod}S` },
    { label: '分割所选片段', key: `${mod}B` },
    { label: '撤销时间线操作', key: `${mod}Z` },
    { label: '重做时间线操作', key: `${mod}Shift+Z` },
    { label: '删除所选片段', key: 'Delete / Backspace' },
    { label: '编辑位置逐帧移动', key: '← / →' },
    { label: '取消正在拖动', key: 'Esc' },
  ]
  const recentItems = recentProjects.slice(0, 5).map((entry) => ({ id: `recent-${entry.path}`, label: entry.name, icon: <Clock3 />, onClick: () => void openRecentProject(entry.path) }))
  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (event.isComposing || !(mac ? event.metaKey : event.ctrlKey) || event.altKey) return
      const key = event.key.toLowerCase()
      if (!['s', 'o', 'n'].includes(key)) return
      const editing = event.target instanceof HTMLElement && (event.target.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(event.target.tagName))
      if (editing || document.querySelector('[aria-modal="true"]')) return
      event.preventDefault()
      if (busy) return
      if (key === 's') void saveProject()
      if (key === 'n') void newProject()
      if (key === 'o') { if (isDesktop) void openProject(); else picker.current?.click() }
    }
    window.addEventListener('keydown', handler)
    return () => window.removeEventListener('keydown', handler)
  }, [busy, mac, newProject, openProject, saveProject])
  useEffect(() => {
    if (!windows) return
    let active = true
    const unlisten: Array<() => void> = []
    const current = getCurrentWindow()
    const reportError = () => { if (active) setStatusMessage('窗口操作未完成，请重试。') }
    const refresh = () => {
      void current.isMaximized().then((value) => { if (active) setMaximized(value) }).catch(reportError)
    }
    const track = (subscription: Promise<() => void>) => {
      void subscription.then((dispose) => { if (active) unlisten.push(dispose); else dispose() }).catch(reportError)
    }
    refresh()
    track(current.onResized(refresh))
    track(current.onFocusChanged(refresh))
    return () => { active = false; unlisten.forEach((dispose) => dispose()) }
  }, [windows, setStatusMessage])
  const runWindowAction = async (action: 'minimize' | 'toggleMaximize' | 'close' | 'startDragging') => {
    if (!windows) return
    try {
      const current = getCurrentWindow()
      // close() preserves the Rust CloseRequested / unsaved-project guard.
      await current[action]()
      if (action === 'toggleMaximize') setMaximized(await current.isMaximized())
    } catch { setStatusMessage('窗口操作未完成，请重试。') }
  }
  const startWindowDrag = (event: ReactMouseEvent<HTMLElement>) => {
    if (!windows || event.button !== 0 || !(event.target instanceof Element)) return
    // Portal events bubble through React but are outside the actual title bar.
    if (!event.currentTarget.contains(event.target) || event.target.closest('button, input, [role="menu"], [data-no-window-drag]')) return
    event.preventDefault()
    void runWindowAction(event.detail === 2 ? 'toggleMaximize' : 'startDragging')
  }
  return (
    <header className="project-toolbar" aria-label="项目工具栏" data-window-chrome={windows || undefined} onMouseDown={startWindowDrag}>
      <div className="project-toolbar__leading">
        <div className="project-toolbar__brand" aria-label="KaiCut / 开剪" title="KaiCut / 开剪">
          <img src="/kaicut-icon.png" alt="" aria-hidden="true" />
          <span>开剪</span>
        </div>
        <DropdownMenu className="project-toolbar__menu" trigger={<Button variant="secondary" className="project-toolbar__menu-trigger" aria-label="项目菜单" title="项目菜单" disabled={busy}><span>菜单</span><ChevronDown size={12} strokeWidth={1.6} /></Button>} items={[
          { id: 'new', label: '新建项目', shortcut: `${mod}N`, icon: <Plus />, disabled: busy, onClick: () => void newProject() },
          { id: 'open', label: '打开项目…', shortcut: `${mod}O`, icon: <FolderOpen />, disabled: busy, onClick: open },
          ...(recentItems.length > 0 ? ['separator' as const, ...recentItems] : []),
          'separator',
          { id: 'save', label: '保存项目', shortcut: `${mod}S`, icon: <Save />, disabled: busy, onClick: () => void saveProject() },
          'separator',
          { id: 'settings', label: '外观设置', icon: <Settings2 />, onClick: onSettings },
        ]} />
        <span className="project-toolbar__status" data-dirty={isDirty} role="status" aria-live="polite">{!isDirty && <CheckCircle2 size={13} strokeWidth={1.7} aria-hidden="true" />}{saveStatus}</span>
      </div>
      <div className="project-toolbar__project">
        <Button variant="ghost" aria-label={`重命名项目：${project.name}`} title={`重命名项目：${project.name}`} disabled={busy} className="project-toolbar__name" onClick={() => { setDraftName(project.name); setRenameOpen(true) }}><span>{project.name}</span></Button>
      </div>
      <nav className="project-toolbar__actions" aria-label="工具栏操作">
        <Button variant="ghost" size="icon" className="project-toolbar__icon" onClick={() => setShortcutsOpen(true)} aria-label="快捷键" title="快捷键"><Keyboard size={16} strokeWidth={1.6} /></Button>
        <DropdownMenu align="right" className="project-toolbar__menu" trigger={<Button variant="ghost" size="icon" className="project-toolbar__icon" aria-label="布局" title="布局"><PanelsTopLeft size={16} strokeWidth={1.6} /></Button>} items={[
          { id: 'library', label: showLibrary ? '隐藏素材库' : '显示素材库', icon: showLibrary ? <Check /> : undefined, onClick: onToggleLibrary },
          { id: 'inspector', label: showInspector ? '隐藏检查器' : '显示检查器', icon: showInspector ? <Check /> : undefined, onClick: onToggleInspector },
          'separator',
          { id: 'reset', label: '恢复默认布局', onClick: onResetLayout },
        ]} />
        <span className="project-toolbar__separator" aria-hidden="true" />
        <Button variant="accent" className="project-toolbar__export" onClick={onExport} disabled={busy} aria-label="导出" title="导出"><Upload size={16} strokeWidth={1.6} /><span>导出</span></Button>
        {windows && <div className="project-toolbar__window-controls" data-no-window-drag="true" aria-label="窗口控制">
          <Button variant="ghost" className="project-toolbar__window-control" onClick={() => void runWindowAction('minimize')} aria-label="最小化" title="最小化"><Minus size={14} strokeWidth={1.6} /></Button>
          <Button variant="ghost" className="project-toolbar__window-control" onClick={() => void runWindowAction('toggleMaximize')} aria-label={maximized ? '还原' : '最大化'} title={maximized ? '还原' : '最大化'}>{maximized ? <Copy size={12} strokeWidth={1.6} /> : <Square size={12} strokeWidth={1.6} />}</Button>
          <Button variant="ghost" className="project-toolbar__window-control project-toolbar__window-control-close" onClick={() => void runWindowAction('close')} aria-label="关闭" title="关闭"><X size={15} strokeWidth={1.6} /></Button>
        </div>}
      </nav>
      <input ref={picker} hidden type="file" accept=".kaicut" onChange={(event) => { const file = event.target.files?.[0]; if (file) void openProjectFile(file); event.currentTarget.value = '' }} />
      <Dialog className="project-toolbar-dialog" open={renameOpen} onOpenChange={setRenameOpen} title="项目名称" confirmText="保存名称" cancelText="取消" confirmDisabled={busy || !draftName.trim()} onConfirm={async () => { await setProjectName(draftName); return useProjectStore.getState().project.name === draftName.trim() }}>
        <Input aria-label="项目名称" data-dialog-autofocus maxLength={120} value={draftName} onChange={(event) => setDraftName(event.target.value)} />
      </Dialog>
      <Dialog className="project-toolbar-dialog" open={shortcutsOpen} onOpenChange={setShortcutsOpen} title="快捷键" description="在工作区使用。编辑文本时，快捷键交给输入框处理。" icon={<Keyboard size={20} />} hideCancel confirmText="完成">
        <dl className="project-toolbar-shortcuts">{shortcuts.map(({ label, key }) => <div key={key}><dt>{label}</dt><dd><kbd>{key}</kbd></dd></div>)}</dl>
      </Dialog>
    </header>
  )
}
