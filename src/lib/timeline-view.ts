import type { TimelineClip, TimelineTrack } from '@/types/project'

export const PRIMARY_VIDEO_TRACK_ID = 'video-1'

export function timelineEnd(tracks: TimelineTrack[]) {
  return tracks.reduce((end, track) => track.clips.reduce((value, clip) => Math.max(value, clip.startFrame + clip.durationFrames), end), 0)
}

export function frameFromOffset(pixels: number, pixelsPerSecond: number, fps: number) {
  return Math.max(0, Math.round(pixels / pixelsPerSecond * fps))
}

export function snapTimelineFrame(frame: number, targets: number[], threshold: number) {
  let snapped = frame
  let distance = Math.max(0, threshold)
  for (const target of targets) {
    const difference = Math.abs(target - frame)
    if (target >= 0 && difference <= distance) { snapped = target; distance = difference }
  }
  return Math.max(0, Math.round(snapped))
}

export function primaryVideoSnapTargets(tracks: TimelineTrack[], clipIds: string | string[]) {
  const excluded = new Set(typeof clipIds === 'string' ? [clipIds] : clipIds)
  const primary = tracks.find((track) => track.id === PRIMARY_VIDEO_TRACK_ID && track.kind === 'video')
  return primary?.clips.filter((clip) => !excluded.has(clip.id)).flatMap((clip) => [clip.startFrame, clip.startFrame + clip.durationFrames]) ?? []
}

/** Move as one rigid selection, including its spacing between same-kind tracks. */
export function moveTimelineClips(tracks: TimelineTrack[], clips: TimelineClip[], anchor: TimelineClip, targetTrackId: string, startFrame: number, maximumFrame: number) {
  const kind = tracks.find((track) => track.id === anchor.trackId)?.kind
  const lanes = tracks.filter((track) => track.kind === kind)
  const shift = lanes.findIndex((track) => track.id === targetTrackId) - lanes.findIndex((track) => track.id === anchor.trackId)
  const destinations = clips.map((clip) => lanes[lanes.findIndex((track) => track.id === clip.trackId) + shift])
  const canShift = destinations.every((track) => track && !track.locked)
  const firstFrame = Math.min(...clips.map((clip) => clip.startFrame))
  const lastFrame = Math.max(...clips.map((clip) => clip.startFrame + clip.durationFrames))
  const delta = Math.max(-firstFrame, Math.min(maximumFrame - lastFrame, Math.round(startFrame - anchor.startFrame)))
  return clips.map((clip, index) => ({ ...clip, startFrame: clip.startFrame + delta, trackId: canShift ? destinations[index].id : clip.trackId }))
}

/** Trim preserves the opposite edge and never reads before/after the source. */
export function trimClip(clip: TimelineClip, edge: 'left' | 'right', delta: number): TimelineClip {
  if (edge === 'left') {
    const movement = Math.max(-clip.sourceInFrame, -clip.startFrame, Math.min(clip.durationFrames - 1, Math.round(delta)))
    return { ...clip, startFrame: clip.startFrame + movement, sourceInFrame: clip.sourceInFrame + movement, durationFrames: clip.durationFrames - movement }
  }
  const durationFrames = Math.max(1, Math.min(clip.sourceDurationFrames - clip.sourceInFrame, clip.durationFrames + Math.round(delta)))
  return { ...clip, durationFrames }
}
