import { describe, expect, it } from 'vitest'
import { DEFAULT_WORKSPACE, workspaceWidths } from './workspace-layout'

describe('resizable editor panes', () => {
  it('preserves the default editing layout', () => {
    expect(workspaceWidths(1280, DEFAULT_WORKSPACE.library, DEFAULT_WORKSPACE.inspector, true, true)).toEqual({ library: 320, inspector: 300 })
  })
  it('keeps at least 300px for the viewer at the desktop minimum width', () => {
    const panes = workspaceWidths(1024, 460, 380, true, true)
    expect(1024 - panes.library - panes.inspector - 32).toBeGreaterThanOrEqual(300)
    expect(panes.library).toBeGreaterThanOrEqual(180)
    expect(panes.inspector).toBeGreaterThanOrEqual(220)
  })
  it('gives collapsed pane space back to the viewer', () => {
    expect(workspaceWidths(1024, 460, 380, false, false)).toEqual({ library: 0, inspector: 0 })
    expect(workspaceWidths(1024, 460, 380, false, true)).toEqual({ library: 0, inspector: 380 })
  })
})
