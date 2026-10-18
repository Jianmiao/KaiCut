export interface PreviewRect { x: number; y: number; width: number; height: number }
export interface PreviewInsets { top: number; right: number; bottom: number; left: number }
export interface NativePreviewGeometry extends PreviewRect { scaleFactor: number; blocked: boolean }
export interface PreviewMeasurement {
  canvas: PreviewRect
  insets: PreviewInsets
  viewport: PreviewRect
  clips: PreviewRect[]
  controls: PreviewRect | null
  scaleFactor: number
  blocked: boolean
}

const validRect = (rect: PreviewRect) => Object.values(rect).every(Number.isFinite) && rect.width > 0 && rect.height > 0
const intersect = (a: PreviewRect, b: PreviewRect): PreviewRect => {
  const x = Math.max(a.x, b.x), y = Math.max(a.y, b.y)
  return { x, y, width: Math.min(a.x + a.width, b.x + b.width) - x, height: Math.min(a.y + a.height, b.y + b.height) - y }
}

/** All coordinates stay in viewport CSS pixels; the native owner applies DPI once. */
export function nativePreviewGeometry(input: PreviewMeasurement): NativePreviewGeometry {
  const scaleFactor = Number.isFinite(input.scaleFactor) && input.scaleFactor > 0 ? input.scaleFactor : 1
  const blocked = (): NativePreviewGeometry => ({ x: 0, y: 0, width: 0, height: 0, scaleFactor, blocked: true })
  if (input.blocked || scaleFactor !== input.scaleFactor || !validRect(input.canvas) || !validRect(input.viewport)
    || !Object.values(input.insets).every((value) => Number.isFinite(value) && value >= 0)) return blocked()
  const { canvas, insets } = input
  let content: PreviewRect = { x: canvas.x + insets.left, y: canvas.y + insets.top,
    width: canvas.width - insets.left - insets.right, height: canvas.height - insets.top - insets.bottom }
  for (const clip of [input.viewport, ...input.clips]) {
    if (!validRect(clip) || !validRect(content)) return blocked()
    content = intersect(content, clip)
  }
  if (input.controls) {
    if (!validRect(input.controls)) return blocked()
    if (validRect(intersect(content, input.controls))) content.height = Math.min(content.height, input.controls.y - content.y)
  }
  return validRect(content) && content.x >= 0 && content.y >= 0 ? { ...content, scaleFactor, blocked: false } : blocked()
}
