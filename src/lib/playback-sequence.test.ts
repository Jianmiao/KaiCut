import { describe, expect, it } from 'vitest'
import { nextContiguousClip } from './playback-sequence'

const current = { id: 'a', mediaId: 'm1', trackId: 'video-1', startFrame: 0, durationFrames: 30, sourceInFrame: 0, sourceDurationFrames: 60 }
describe('timeline playback sequence', () => {
  it('advances only to a clip that starts exactly at the current end', () => {
    const next = { ...current, id: 'b', mediaId: 'm2', startFrame: 30 }
    expect(nextContiguousClip(current, [next])).toEqual(next)
    expect(nextContiguousClip(current, [{ ...next, startFrame: 31 }])).toBeUndefined()
  })
})
