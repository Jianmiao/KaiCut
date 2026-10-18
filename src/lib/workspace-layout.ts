export const DEFAULT_WORKSPACE = { library: 320, inspector: 300, timeline: 300 } as const
export const WORKSPACE_LIMITS = { library: [180, 460], inspector: [220, 380], timeline: [220, 520] } as const

/** Keep the viewer usable when a previously wide layout is resized. */
export function workspaceWidths(width: number, library: number, inspector: number, showLibrary: boolean, showInspector: boolean) {
  const available = Math.max(0, width - 16 - (showLibrary ? 8 : 0) - (showInspector ? 8 : 0) - 300)
  let left = showLibrary ? Math.max(180, Math.min(460, library)) : 0
  let right = showInspector ? Math.max(220, Math.min(380, inspector)) : 0
  let excess = Math.max(0, left + right - available)
  const reduceLeft = Math.min(excess, Math.max(0, left - 180))
  left -= reduceLeft; excess -= reduceLeft
  right -= Math.min(excess, Math.max(0, right - 220))
  return { library: left, inspector: right }
}
