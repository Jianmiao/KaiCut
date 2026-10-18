import { describe, expect, it } from 'vitest'
import { nativePreviewGeometry } from './native-preview-host'

const input = {
  canvas: { x: 100, y: 70, width: 640, height: 420 },
  insets: { top: 1, right: 9, bottom: 53, left: 9 },
  viewport: { x: 0, y: 0, width: 1200, height: 800 },
  clips: [] as Array<{ x: number; y: number; width: number; height: number }>,
  controls: null as { x: number; y: number; width: number; height: number } | null,
  scaleFactor: 1.5,
  blocked: false,
}

describe('native preview client geometry', () => {
  it('uses the content box without scaling CSS pixels a second time', () => {
    expect(nativePreviewGeometry(input)).toEqual({ x: 109, y: 71, width: 622, height: 366, scaleFactor: 1.5, blocked: false })
  })
  it('intersects content with viewport and ancestor clipping then excludes enlarged controls', () => {
    expect(nativePreviewGeometry({ ...input, viewport: { x: 0, y: 0, width: 600, height: 800 },
      clips: [{ x: 150, y: 100, width: 700, height: 500 }],
      controls: { x: 108, y: 390, width: 620, height: 84 },
    })).toEqual({ x: 150, y: 100, width: 450, height: 290, scaleFactor: 1.5, blocked: false })
  })
  it('blocks a modal, invalid measurements and invisible content with a zero rectangle', () => {
    for (const changes of [
      { blocked: true }, { scaleFactor: Number.NaN }, { scaleFactor: 0 },
      { canvas: { ...input.canvas, width: -1 } }, { canvas: { ...input.canvas, height: 0 } },
      { canvas: { ...input.canvas, x: Number.NaN } },
      { insets: { ...input.insets, bottom: -1 } },
      { clips: [{ x: 900, y: 700, width: 1, height: 1 }] },
      { controls: { x: 100, y: 70, width: 640, height: 420 } },
    ]) expect(nativePreviewGeometry({ ...input, ...changes })).toEqual(expect.objectContaining({ x: 0, y: 0, width: 0, height: 0, blocked: true }))
  })
  it('clips offscreen origins and does not crop for unrelated controls', () => {
    expect(nativePreviewGeometry({ ...input, canvas: { x: -20, y: -30, width: 640, height: 420 },
      controls: { x: 1000, y: 100, width: 100, height: 40 },
    })).toEqual({ x: 0, y: 0, width: 611, height: 337, scaleFactor: 1.5, blocked: false })
  })
})
