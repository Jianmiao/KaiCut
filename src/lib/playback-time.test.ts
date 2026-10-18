import { describe, expect, it } from 'vitest'
import { clampClipPlaybackSeconds, clipPlaybackFrame, clipSourceEndSeconds, clipSourceStartSeconds } from './playback-time'

const clip = { id: 'c', mediaId: 'm', trackId: 'video-1', startFrame: 30, sourceInFrame: 60, durationFrames: 90, sourceDurationFrames: 180 }

describe('clip playback time mapping', () => {
  it('maps source seconds into project frames', () => {
    expect(clipSourceStartSeconds(clip, 30)).toBe(2)
    expect(clipSourceEndSeconds(clip, 30)).toBe(5)
    expect(clipPlaybackFrame(clip, 3, 30)).toBe(60)
  })

  it('clamps playback to the selected clip source range', () => {
    expect(clampClipPlaybackSeconds(clip, 0, 30)).toBe(2)
    expect(clampClipPlaybackSeconds(clip, 9, 30)).toBe(5)
    expect(clipPlaybackFrame(clip, 99, 30)).toBe(120)
  })
})
