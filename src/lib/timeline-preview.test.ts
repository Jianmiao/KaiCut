import { describe, expect, it } from 'vitest'
import fixture from '@/test/fixtures/preview-parity.json'
import type { MediaAsset } from '@/types/project'
import { createEmptyProject, parseProject } from './kaicut-format'
import { resolveTimelineFrame } from './timeline-preview'

function projectAndMedia() {
  const project = parseProject(JSON.stringify(fixture.project))
  // Source fps does not change offsets measured in project-timebase frames.
  const media: MediaAsset[] = project.mediaRefs.map((asset) => ({ ...asset, objectUrl: `blob:${asset.id}`, probe: { state: 'ready', fpsNumerator: 60, fpsDenominator: 1 } }))
  return { project, media }
}

describe('preview/export frame parity', () => {
  it.each(fixture.frames)('resolves frame $frame against the shared export oracle', (expected) => {
    const { project, media } = projectAndMedia()
    const result = resolveTimelineFrame(project, media, expected.frame)
    expect(result.durationFrames).toBe(fixture.durationFrames)
    expect(result.visual?.clip.id ?? null).toBe(expected.visual?.clipId ?? null)
    if (expected.visual) {
      expect(result.visual?.sourceSeconds).toBeCloseTo(expected.visual.sourceSeconds, 9)
      expect(result.visual?.track.kind).toBe('video')
      expect(result.visual?.asset?.id).toBe(result.visual?.clip.mediaId)
    }
    expect(result.audio.map(({ clip }) => clip.id)).toEqual(expected.audio.map(({ clipId }) => clipId))
    result.audio.forEach((audio, index) => {
      expect(audio.sourceSeconds).toBeCloseTo(expected.audio[index].sourceSeconds, 9)
      expect(audio.gain).toBeCloseTo(expected.audio[index].gain, 9)
      expect(audio.asset?.id).toBe(audio.clip.mediaId)
    })
  })

  it('keeps unavailable upper media selected instead of exposing the lower video', () => {
    const { project, media } = projectAndMedia()
    const result = resolveTimelineFrame(project, media.filter((asset) => asset.id !== 'image'), 30)
    expect(result.visual?.clip.id).toBe('image-clip')
    expect(result.visual?.asset).toBeUndefined()
  })

  it('preserves a missing asset error for the renderer instead of falling through', () => {
    const { project, media } = projectAndMedia()
    media[0].linkState = 'missing'
    media[0].objectUrl = undefined
    const result = resolveTimelineFrame(project, media, 30)
    expect(result.visual?.asset?.linkState).toBe('missing')
    expect(result.visual?.clip.id).toBe('image-clip')
  })

  it('hides muted video lanes and resolves the next visible lane at the same source time', () => {
    const { project, media } = projectAndMedia()
    project.tracks[0].muted = true
    expect(resolveTimelineFrame(project, media, 30).visual?.clip.id).toBe('lower-clip')
    expect(resolveTimelineFrame(project, media, 30).visual?.sourceSeconds).toBeCloseTo(1.4014, 9)
    project.tracks[1].muted = true
    expect(resolveTimelineFrame(project, media, 30).visual).toBeUndefined()
  })

  it('uses project lane order rather than primary-track id or media-library order', () => {
    const { project, media } = projectAndMedia()
    const [upper, lower, ...audio] = project.tracks
    project.tracks = [lower, upper, ...audio]
    expect(resolveTimelineFrame(project, [...media].reverse(), 30).visual?.clip.id).toBe('lower-clip')
  })

  it('mixes independent audio lanes without adding occluded video audio', () => {
    const { project, media } = projectAndMedia()
    project.tracks[3].muted = false
    const result = resolveTimelineFrame(project, media, 30)
    expect(result.audio.map(({ clip }) => clip.id)).toEqual(['music-clip', 'muted-clip'])
    expect(result.audio.map(({ gain }) => gain)).toEqual([0.6, 1])
    expect(result.visual?.asset?.kind).toBe('image')
  })

  it('retains an active zero-volume lane so readiness can be tracked', () => {
    const { project, media } = projectAndMedia()
    project.tracks[2].volume = 0
    const result = resolveTimelineFrame(project, media, 30)
    expect(result.audio.map(({ clip, gain }) => [clip.id, gain])).toEqual([['music-clip', 0]])
  })

  it('returns no stale media before the timeline or for an empty project', () => {
    const { project, media } = projectAndMedia()
    expect(resolveTimelineFrame(project, media, -1)).toEqual({ durationFrames: 120, audio: [] })
    expect(resolveTimelineFrame(createEmptyProject(), [], 0)).toEqual({ durationFrames: 0, audio: [] })
  })
})
