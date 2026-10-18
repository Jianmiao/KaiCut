import type { TimelineClip } from '@/types/project'

export function clipSourceStartSeconds(clip: TimelineClip, fps: number) {
  return clip.sourceInFrame / fps
}

export function clipSourceEndSeconds(clip: TimelineClip, fps: number) {
  return (clip.sourceInFrame + clip.durationFrames) / fps
}

export function clipPlaybackFrame(clip: TimelineClip, seconds: number, fps: number) {
  const sourceFrame = Math.max(clip.sourceInFrame, Math.round(seconds * fps))
  return clip.startFrame + Math.min(clip.durationFrames, sourceFrame - clip.sourceInFrame)
}

export function clampClipPlaybackSeconds(clip: TimelineClip, seconds: number, fps: number) {
  return Math.max(clipSourceStartSeconds(clip, fps), Math.min(clipSourceEndSeconds(clip, fps), seconds))
}
