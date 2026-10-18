/** Source duration, not a frame-accurate editing timecode. */
export function formatMediaDuration(milliseconds?: number) {
  if (milliseconds === undefined || !Number.isFinite(milliseconds) || milliseconds < 0) return '—'
  const ms = Math.floor(milliseconds)
  const seconds = Math.floor(ms / 1000)
  return `${Math.floor(seconds / 3600).toString().padStart(2, '0')}:${Math.floor(seconds / 60 % 60).toString().padStart(2, '0')}:${(seconds % 60).toString().padStart(2, '0')}.${(ms % 1000).toString().padStart(3, '0')}`
}
