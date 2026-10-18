import type { KaicutProject, MediaAsset, TimelineClip, TimelineTrack } from '@/types/project'
import { audioGainAtFrame } from './audio-mix'
import { timelineEnd } from './timeline-view'

export interface TimelineFrameSource {
  clip: TimelineClip
  track: TimelineTrack
  asset?: MediaAsset
  sourceSeconds: number
}

export interface TimelineFrame {
  durationFrames: number
  visual?: TimelineFrameSource
  /** Independent audio lanes; embedded audio belongs to the visible video. */
  audio: Array<TimelineFrameSource & { gain: number }>
}

/** Resolve project-timebase frames using the same opaque-lane rules as export. */
export function resolveTimelineFrame(project: KaicutProject, media: MediaAsset[], frame: number): TimelineFrame {
  const result: TimelineFrame = { durationFrames: timelineEnd(project.tracks), audio: [] }
  const assets = new Map(media.map((asset) => [asset.id, asset]))
  const { fpsNumerator, fpsDenominator } = project.timebase

  for (const track of project.tracks) {
    if (track.muted) continue
    for (const clip of track.clips) {
      if (!(frame >= clip.startFrame && frame < clip.startFrame + clip.durationFrames)) continue
      const source: TimelineFrameSource = {
        clip,
        track,
        asset: assets.get(clip.mediaId),
        sourceSeconds: (clip.sourceInFrame + frame - clip.startFrame) * fpsDenominator / fpsNumerator,
      }
      if (track.kind === 'video') {
        // Missing upper media remains selected: never show a lower lane that
        // the export would occlude, or silently turn a missing source into it.
        result.visual ??= source
      } else {
        // Keep active zero-gain clips (fade edge/volume=0) for media readiness.
        result.audio.push({ ...source, gain: audioGainAtFrame(track, clip, frame) })
      }
    }
  }
  return result
}
