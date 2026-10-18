import type { TimelineClip } from '@/types/project'

/** HTML media presentation time, not a native audio-device sample clock. */
export function mediaProjectFrame(clip: TimelineClip, seconds: number, fps: number) {
  return clip.startFrame + seconds * fps - clip.sourceInFrame
}

export function continuousSource(previous: TimelineClip | undefined, next: TimelineClip | undefined) {
  return !!previous && !!next && previous.mediaId === next.mediaId &&
    previous.startFrame + previous.durationFrames === next.startFrame &&
    previous.sourceInFrame + previous.durationFrames === next.sourceInFrame
}

export function syncAdjustment(current: number, desired: number, state: { playing: boolean; master: boolean; seeking: boolean; force: boolean }): { rate: number; seek?: number } {
  if (state.seeking) return { rate: 1 }
  const drift = desired - current
  if ((state.force || !state.playing) && Math.abs(drift) > 0.001) return { rate: 1, seek: desired }
  if (state.master || !state.playing) return { rate: 1 }
  if (Math.abs(drift) > 0.25) return { rate: 1, seek: desired }
  if (Math.abs(drift) <= 0.02) return { rate: 1 }
  return { rate: Math.max(0.97, Math.min(1.03, 1 + drift * 0.3)) }
}
