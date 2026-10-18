import type { TimelineClip, TimelineTrack } from '@/types/project'

export function activeAudioClips(tracks: Array<{ kind: 'video' | 'audio'; clips: TimelineClip[] }>, frame: number) {
  return tracks.filter((track) => track.kind === 'audio').flatMap((track) => track.clips.filter((clip) => frame >= clip.startFrame && frame < clip.startFrame + clip.durationFrames))
}

export function audioSourceSeconds(clip: TimelineClip, frame: number, fps: number) {
  return (clip.sourceInFrame + Math.max(0, frame - clip.startFrame)) / fps
}

/**
 * Returns the linear gain for one audio clip at a project frame.
 * Keeping this calculation in the UI projection lets the preview update at
 * frame cadence without making an IPC round trip for every audio tick. Rust
 * exposes the same calculation for native callers and export integration.
 */
export function audioGainAtFrame(track: Pick<TimelineTrack, 'kind' | 'muted' | 'volume'>, clip: Pick<TimelineClip, 'startFrame' | 'durationFrames' | 'fadeInFrames' | 'fadeOutFrames'>, frame: number) {
  if (track.kind !== 'audio' || track.muted || frame < clip.startFrame || frame >= clip.startFrame + clip.durationFrames) return 0
  const local = frame - clip.startFrame
  let gain = Math.max(0, Math.min(1, track.volume))
  const fadeInFrames = clip.fadeInFrames ?? 0
  const fadeOutFrames = clip.fadeOutFrames ?? 0
  if (fadeInFrames > 0) gain *= Math.max(0, Math.min(1, local / fadeInFrames))
  if (fadeOutFrames > 0) {
    const remaining = Math.max(0, clip.durationFrames - local - 1)
    gain *= Math.max(0, Math.min(1, remaining / fadeOutFrames))
  }
  return Math.max(0, Math.min(1, gain))
}

export function audioDriftSeconds(currentSeconds: number, desiredSeconds: number) {
  return currentSeconds - desiredSeconds
}

export function shouldResyncAudio(currentSeconds: number, desiredSeconds: number, threshold = 0.06) {
  return Math.abs(audioDriftSeconds(currentSeconds, desiredSeconds)) > threshold
}
