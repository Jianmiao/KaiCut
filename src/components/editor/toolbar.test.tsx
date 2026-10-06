import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { Toolbar } from './toolbar'

const fixture = vi.hoisted(() => ({
  desktop: true,
  maximized: false,
  resizeListeners: new Set<(event: { payload: { width: number; height: number } }) => void>(),
  focusListeners: new Set<(event: { payload: boolean }) => void>(),
  project: {
    project: { name: '标题栏测试' },
    projectPath: null,
    isDirty: true,
    busy: false,
    newProject: vi.fn(),
    openProject: vi.fn(),
    openProjectFile: vi.fn(),
    saveProject: vi.fn(),
    setProjectName: vi.fn(),
    setStatusMessage: vi.fn(),
  },
  window: {
    minimize: vi.fn<() => Promise<void>>(),
    toggleMaximize: vi.fn<() => Promise<void>>(),
    close: vi.fn<() => Promise<void>>(),
    destroy: vi.fn<() => Promise<void>>(),
    startDragging: vi.fn<() => Promise<void>>(),
    isMaximized: vi.fn<() => Promise<boolean>>(),
    onResized: vi.fn<(listener: (event: { payload: { width: number; height: number } }) => void) => Promise<() => void>>(),
    onFocusChanged: vi.fn<(listener: (event: { payload: boolean }) => void) => Promise<() => void>>(),
  },
  getCurrentWindow: vi.fn(),
}))

vi.mock('@/stores/project-store', () => ({
  get isDesktop() { return fixture.desktop },
  useProjectStore: Object.assign(() => fixture.project, { getState: () => fixture.project }),
}))
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: fixture.getCurrentWindow }))

let root: Root
let host: HTMLDivElement
let platformSpy: ReturnType<typeof vi.spyOn>

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true })
  vi.resetAllMocks()
  fixture.desktop = true
  fixture.maximized = false
  fixture.resizeListeners.clear()
  fixture.focusListeners.clear()
  fixture.getCurrentWindow.mockReturnValue(fixture.window)
  fixture.window.minimize.mockResolvedValue(undefined)
  fixture.window.toggleMaximize.mockResolvedValue(undefined)
  fixture.window.close.mockResolvedValue(undefined)
  fixture.window.destroy.mockResolvedValue(undefined)
  fixture.window.startDragging.mockResolvedValue(undefined)
  fixture.window.isMaximized.mockImplementation(async () => fixture.maximized)
  fixture.window.onResized.mockImplementation(async (listener) => {
    fixture.resizeListeners.add(listener)
    return () => { fixture.resizeListeners.delete(listener) }
  })
  fixture.window.onFocusChanged.mockImplementation(async (listener) => {
    fixture.focusListeners.add(listener)
    return () => { fixture.focusListeners.delete(listener) }
  })
  platformSpy = vi.spyOn(window.navigator, 'platform', 'get').mockReturnValue('Win32')
  host = document.createElement('div')
  host.id = 'root'
  document.body.appendChild(host)
  root = createRoot(host)
})

afterEach(async () => {
  await act(() => root.unmount())
  host.remove()
  platformSpy.mockRestore()
})

async function renderToolbar() {
  await act(() => root.render(<Toolbar onExport={vi.fn()} onSettings={vi.fn()}
    showLibrary showInspector onToggleLibrary={vi.fn()} onToggleInspector={vi.fn()} onResetLayout={vi.fn()} />))
}

function button(label: string | RegExp) {
  const match = Array.from(document.querySelectorAll<HTMLButtonElement>('button')).find((element) => {
    const name = element.getAttribute('aria-label') ?? element.textContent ?? ''
    return typeof label === 'string' ? name === label : label.test(name)
  })
  expect(match, `button ${String(label)} exists`).toBeDefined()
  return match!
}

function toolbar() { return host.querySelector<HTMLElement>('[aria-label="项目工具栏"]')! }
async function mouseDown(target: Element, detail = 1, mouseButton = 0) {
  await act(() => {
    target.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true, button: mouseButton, detail }))
  })
}
async function click(target: HTMLElement) { await act(() => target.click()) }

describe('native Windows title bar', () => {
  it('minimizes, toggles maximization and requests close without destroying the window', async () => {
    await renderToolbar()
    await click(button('最小化'))
    await click(button('最大化'))
    await click(button('关闭'))
    expect(fixture.window.minimize).toHaveBeenCalledTimes(1)
    expect(fixture.window.toggleMaximize).toHaveBeenCalledTimes(1)
    expect(fixture.window.close).toHaveBeenCalledTimes(1)
    // close emits the existing close-request event so unsaved work can be protected.
    expect(fixture.window.destroy).not.toHaveBeenCalled()
    expect(fixture.window.startDragging).not.toHaveBeenCalled()
  })

  it.each([
    { desktop: false, platform: 'Win32', label: 'Windows browser preview' },
    { desktop: true, platform: 'MacIntel', label: 'macOS desktop' },
    { desktop: true, platform: 'Linux x86_64', label: 'Linux desktop' },
  ])('does not show or operate native Windows controls in $label', async ({ desktop, platform }) => {
    fixture.desktop = desktop
    platformSpy.mockReturnValue(platform)
    await renderToolbar()
    expect(host.querySelector('[aria-label="窗口控制"]')).toBeNull()
    expect(host.querySelector('[aria-label="最小化"]')).toBeNull()
    await mouseDown(toolbar())
    await mouseDown(toolbar(), 2)
    expect(fixture.getCurrentWindow).not.toHaveBeenCalled()
  })

  it('reads the initial maximized state and refreshes its restore label on native resize and focus', async () => {
    fixture.maximized = true
    await renderToolbar()
    expect(button(/^(还原|恢复)(窗口)?$/)).toBeDefined()
    expect(host.querySelector('[aria-label="最大化"]')).toBeNull()
    expect(fixture.resizeListeners.size).toBe(1)
    expect(fixture.focusListeners.size).toBe(1)

    fixture.maximized = false
    await act(() => {
      fixture.resizeListeners.forEach((listener) => listener({ payload: { width: 1280, height: 820 } }))
    })
    expect(button('最大化')).toBeDefined()

    fixture.maximized = true
    await act(() => { fixture.focusListeners.forEach((listener) => listener({ payload: true })) })
    expect(button(/^(还原|恢复)(窗口)?$/)).toBeDefined()
    expect(fixture.window.isMaximized).toHaveBeenCalledTimes(3)
  })

  it('unsubscribes from native window events when the toolbar is removed', async () => {
    await renderToolbar()
    await act(() => root.render(null))
    expect(fixture.resizeListeners.size).toBe(0)
    expect(fixture.focusListeners.size).toBe(0)
  })

  it('drags from blank chrome, brand descendants and status but ignores other mouse buttons', async () => {
    await renderToolbar()
    expect(toolbar().hasAttribute('data-tauri-drag-region')).toBe(false)
    await mouseDown(toolbar())
    await mouseDown(host.querySelector('.project-toolbar__brand img')!)
    await mouseDown(host.querySelector('.project-toolbar__status')!)
    expect(fixture.window.startDragging).toHaveBeenCalledTimes(3)
    await mouseDown(toolbar(), 1, 1)
    await mouseDown(toolbar(), 1, 2)
    expect(fixture.window.startDragging).toHaveBeenCalledTimes(3)
    expect(fixture.window.toggleMaximize).not.toHaveBeenCalled()
  })

  it('handles a double-click on blank chrome as a single maximize toggle, without starting a drag', async () => {
    await renderToolbar()
    await mouseDown(toolbar(), 2)
    await act(() => { toolbar().dispatchEvent(new MouseEvent('dblclick', { bubbles: true, detail: 2 })) })
    expect(fixture.window.toggleMaximize).toHaveBeenCalledTimes(1)
    expect(fixture.window.startDragging).not.toHaveBeenCalled()
  })

  it('never drags from controls, their icons, file inputs or an open menu', async () => {
    await renderToolbar()
    await mouseDown(button('导出'))
    await mouseDown(button('导出').querySelector('svg')!, 2)
    await mouseDown(button('最大化'))
    await mouseDown(host.querySelector('input[type="file"]')!)
    await click(button('项目菜单'))
    const menu = document.querySelector<HTMLElement>('[role="menu"]')!
    await mouseDown(menu)
    await mouseDown(menu.querySelector('[role="separator"]')!, 2)
    await mouseDown(menu.querySelector('[role="menuitem"]')!)
    expect(fixture.window.startDragging).not.toHaveBeenCalled()
    expect(fixture.window.toggleMaximize).not.toHaveBeenCalled()
  })

  it('does not drag or maximize through a portal dialog or its backdrop', async () => {
    await renderToolbar()
    await click(button(/^重命名项目：/))
    const dialog = document.querySelector<HTMLElement>('[role="dialog"]')!
    expect(dialog).not.toBeNull()
    expect(toolbar().contains(dialog)).toBe(false)
    // React portal events still bubble to the toolbar owner, unlike DOM ancestry.
    await mouseDown(dialog)
    await mouseDown(dialog.querySelector('h2')!, 2)
    await mouseDown(dialog.querySelector('input')!)
    await mouseDown(dialog.parentElement!)
    await mouseDown(dialog.parentElement!, 2)
    expect(fixture.window.startDragging).not.toHaveBeenCalled()
    expect(fixture.window.toggleMaximize).not.toHaveBeenCalled()
  })

  it.each([
    { operation: 'minimize' as const, label: '最小化' },
    { operation: 'toggleMaximize' as const, label: '最大化' },
    { operation: 'close' as const, label: '关闭' },
  ])('reports a rejected $operation IPC operation to the project status', async ({ operation, label }) => {
    fixture.window[operation].mockRejectedValueOnce(new Error('IPC unavailable'))
    await renderToolbar()
    await click(button(label))
    expect(fixture.project.setStatusMessage).toHaveBeenCalledWith(expect.stringContaining('窗口操作未完成'))
    expect(fixture.window.destroy).not.toHaveBeenCalled()
  })

  it('reports a rejected drag request instead of leaving an unhandled promise', async () => {
    fixture.window.startDragging.mockRejectedValueOnce(new Error('drag unavailable'))
    await renderToolbar()
    await mouseDown(toolbar())
    expect(fixture.project.setStatusMessage).toHaveBeenCalledWith(expect.stringContaining('窗口操作未完成'))
  })
})
