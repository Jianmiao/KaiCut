import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import App from './App'
import { useProjectStore } from '@/stores/project-store'
import { createEmptyProject } from '@/lib/kaicut-format'

const renders = vi.hoisted(() => ({ library: vi.fn(), toolbar: vi.fn(), inspector: vi.fn() }))
vi.mock('@tauri-apps/api/core', () => ({ isTauri: () => false, invoke: vi.fn() }))
vi.mock('@/hooks/use-mcp-bridge', () => ({ useMcpBridge: () => {} }))
vi.mock('@/hooks/use-preferences', () => ({ useApplyPreferences: () => {} }))
vi.mock('@/components/editor/media-bin', () => ({ MediaBin: () => { renders.library(); return <div>Media library</div> } }))
vi.mock('@/components/editor/toolbar', () => ({ Toolbar: () => { renders.toolbar(); return <header>Toolbar</header> } }))
vi.mock('@/components/editor/inspector', () => ({ Inspector: () => { renders.inspector(); return <aside>Inspector</aside> } }))
vi.mock('@/components/editor/preview-area', () => ({ PreviewArea: () => null }))
vi.mock('@/components/editor/timeline', () => ({ Timeline: () => null }))
vi.mock('@/components/editor/export-preflight-dialog', () => ({ ExportPreflightDialog: () => null }))
vi.mock('@/components/ui/dialog', () => ({ Dialog: () => null }))
vi.mock('@/components/ui/sheet', () => ({ Sheet: () => null }))
let root: Root
let host: HTMLDivElement
beforeEach(async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true })
  vi.stubGlobal('ResizeObserver', class { observe() {} disconnect() {} })
  Object.values(renders).forEach((mock) => mock.mockClear())
  useProjectStore.setState({ project: createEmptyProject(), media: [], revision: 0, playheadFrame: 0, isDirty: false, busy: false, statusMessage: 'ready' })
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
  await act(async () => root.render(<App />))
})
afterEach(async () => { await act(async () => root.unmount()); host.remove(); vi.unstubAllGlobals() })

describe('workspace store subscription', () => {
  it('does not rerender unrelated panels for playback ticks but still displays status changes', async () => {
    const initial = Object.fromEntries(Object.entries(renders).map(([name, mock]) => [name, mock.mock.calls.length]))
    for (let frame = 1; frame <= 30; frame++) await act(async () => useProjectStore.getState().setPlayheadFrame(frame))
    for (const [name, mock] of Object.entries(renders)) expect(mock.mock.calls.length).toBe(initial[name])
    await act(async () => useProjectStore.setState({ statusMessage: 'new status' }))
    expect(host.querySelector('.app-status')?.textContent).toContain('new status')
  })
})
