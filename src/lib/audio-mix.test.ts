import { describe, expect, it } from 'vitest'
import { activeAudioClips, audioDriftSeconds, audioGainAtFrame, audioSourceSeconds, shouldResyncAudio } from './audio-mix'

const clip = { id: 'a', mediaId: 'm', trackId: 'audio-1', startFrame: 30, durationFrames: 60, sourceInFrame: 15, sourceDurationFrames: 90, fadeInFrames: 0, fadeOutFrames: 0 }
describe('timeline audio clock', () => {
  it('finds audio clips active at a project frame', () => {
    expect(activeAudioClips([{ kind: 'audio', clips: [clip] }], 30)).toEqual([clip])
    expect(activeAudioClips([{ kind: 'audio', clips: [clip] }], 90)).toEqual([])
  })
  it('keeps overlapping clips from multiple audio tracks in deterministic track order', () => {
    const second = { ...clip, id: 'b', trackId: 'audio-2', startFrame: 0, durationFrames: 120 }
    const video = { kind: 'video' as const, clips: [clip] }
    expect(activeAudioClips([video, { kind: 'audio', clips: [clip] }, { kind: 'audio', clips: [second] }], 30).map((item) => item.id)).toEqual(['a', 'b'])
  })
  it('maps a project frame into source time', () => {
    expect(audioSourceSeconds(clip, 60, 30)).toBe(1.5)
  })
  it('maps the first and last audible frame inside the source range', () => {
    expect(audioSourceSeconds(clip, clip.startFrame, 30)).toBe(0.5)
    expect(audioSourceSeconds(clip, clip.startFrame + clip.durationFrames - 1, 30)).toBeCloseTo(2.4666667)
    expect(audioSourceSeconds(clip, clip.startFrame + clip.durationFrames, 30)).toBe(2.5)
  })
  it('only resyncs audible tracks after meaningful drift', () => {
    expect(audioDriftSeconds(1.02, 1)).toBeCloseTo(0.02)
    expect(shouldResyncAudio(1.02, 1)).toBe(false)
    expect(shouldResyncAudio(1.08, 1)).toBe(true)
  })
  it('applies track volume, mute, and clip fades at project frames', () => {
    const track = { kind: 'audio' as const, muted: false, volume: 0.8 }
    const faded = { ...clip, fadeInFrames: 10, fadeOutFrames: 10 }
    expect(audioGainAtFrame(track, faded, 30)).toBe(0)
    expect(audioGainAtFrame(track, faded, 31)).toBeCloseTo(0.08)
    // Frame 39 is the last fade-in frame and is therefore 90% of track gain.
    expect(audioGainAtFrame(track, faded, 39)).toBeCloseTo(0.72)
    expect(audioGainAtFrame(track, faded, 89)).toBeCloseTo(0)
    expect(audioGainAtFrame({ ...track, muted: true }, faded, 39)).toBe(0)
  })
})
