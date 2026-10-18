import { describe, expect, it } from 'vitest'
import { continuousSource, mediaProjectFrame, syncAdjustment } from './preview-clock'

const clip = { id: 'first', mediaId: 'm', trackId: 'a', startFrame: 30, durationFrames: 60, sourceInFrame: 15, sourceDurationFrames: 120 }
describe('preview presentation clock', () => {
  it('maps source seconds without quantizing fractional project time', () => {
    expect(mediaProjectFrame(clip, 1.01, 30)).toBeCloseTo(45.3)
    expect(mediaProjectFrame(clip, 1.001, 30000 / 1001)).toBeCloseTo(45)
  })
  it('reuses only consecutive timeline and source spans', () => {
    expect(continuousSource(clip, { ...clip, id: 'next', startFrame: 90, sourceInFrame: 75 })).toBe(true)
    expect(continuousSource(clip, { ...clip, id: 'next', startFrame: 90, sourceInFrame: 0 })).toBe(false)
  })
  it('gently corrects slaves, keeps masters at unity, and avoids repeated pending seeks', () => {
    expect(syncAdjustment(1, 1.1, { playing: true, master: false, seeking: false, force: false })).toEqual({ rate: 1.03 })
    expect(syncAdjustment(1.1, 1, { playing: true, master: false, seeking: false, force: false })).toEqual({ rate: 0.97 })
    expect(syncAdjustment(1, 4, { playing: true, master: true, seeking: false, force: false })).toEqual({ rate: 1 })
    expect(syncAdjustment(1, 4, { playing: true, master: false, seeking: true, force: true })).toEqual({ rate: 1 })
    expect(syncAdjustment(1, 4, { playing: true, master: false, seeking: false, force: false })).toEqual({ rate: 1, seek: 4 })
    expect(syncAdjustment(1, 1.1, { playing: false, master: true, seeking: false, force: false })).toEqual({ rate: 1, seek: 1.1 })
  })
})
