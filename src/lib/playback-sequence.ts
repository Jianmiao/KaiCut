import type { TimelineClip } from '@/types/project'

export function nextContiguousClip(current: TimelineClip, clips: TimelineClip[]) {
  const end = current.startFrame + current.durationFrames
  return clips
    .filter((clip) => clip.id !== current.id && clip.startFrame === end)
    .sort((left, right) => left.startFrame - right.startFrame)[0]
}
