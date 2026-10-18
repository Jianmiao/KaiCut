export type MediaKind = 'video' | 'audio' | 'image'
export type MediaLinkState = 'unknown' | 'online' | 'missing' | 'changed'
export type MediaProbeState = 'idle' | 'probing' | 'ready' | 'missing' | 'unlinked' | 'failed'

export interface MediaProbe {
  state: MediaProbeState
  durationMs?: number
  width?: number
  height?: number
  fpsNumerator?: number
  fpsDenominator?: number
  audioSampleRate?: number
  audioChannels?: number
  rotation?: number
  error?: string
}

export interface MediaThumbnail {
  state: 'loading' | 'ready' | 'failed' | 'missing' | 'changed' | 'unsupported'
  dataUrl?: string
  error?: string
}

export interface MediaThumbnailResult extends Omit<MediaThumbnail, 'state'> {
  mediaId: string
  state: Exclude<MediaThumbnail['state'], 'loading'>
}

export interface MediaWaveform {
  state: 'loading' | 'ready' | 'failed' | 'missing' | 'changed' | 'unsupported'
  dataUrl?: string
  error?: string
}

export interface MediaWaveformResult extends Omit<MediaWaveform, 'state'> {
  mediaId: string
  state: Exclude<MediaWaveform['state'], 'loading'>
}

export interface MediaAsset {
  id: string
  name: string
  kind: MediaKind
  extension: string
  size: number
  lastModified: number
  sourcePath?: string
  objectUrl?: string
  linkState?: MediaLinkState
  linkReason?: string
  probe?: MediaProbe
  thumbnail?: MediaThumbnail
  waveform?: MediaWaveform
}

export type PersistedMediaAsset = Omit<MediaAsset, 'objectUrl' | 'linkState' | 'linkReason' | 'probe' | 'thumbnail' | 'waveform'>

export interface TimelineClip {
  id: string
  mediaId: string
  trackId: string
  startFrame: number
  durationFrames: number
  /** Source offsets and duration use the project timebase at 1x speed. */
  sourceInFrame: number
  sourceDurationFrames: number
  /** Audio-only fade durations in project-timebase frames. */
  /** Defaults to 0 when reading a v2 project created before audio fades. */
  fadeInFrames?: number
  fadeOutFrames?: number
}

export type TimelineEdit =
  | { type: 'add'; mediaId: string; trackId: string }
  | { type: 'split'; clipId: string; frame: number }
  | { type: 'delete'; clipIds: string[] }
  | { type: 'rippleDelete'; clipIds: string[] }
  | { type: 'duplicate'; clipId: string }
  | { type: 'move'; clipId: string; trackId: string; startFrame: number }
  | { type: 'moveMany'; moves: Array<{ clipId: string; trackId: string; startFrame: number }> }
  | { type: 'trim'; clipId: string; sourceInFrame: number; startFrame: number; durationFrames: number }
  | { type: 'trackVolume'; trackId: string; volume: number }
  | { type: 'clipFade'; clipId: string; fadeInFrames: number; fadeOutFrames: number }
  | { type: 'trackState'; trackId: string; locked: boolean; muted: boolean }

export interface TimelineTrack {
  id: string
  name: string
  kind: 'video' | 'audio'
  locked: boolean
  muted: boolean
  /** Linear audio track gain. Video tracks must keep the default value 1. */
  volume: number
  clips: TimelineClip[]
}

export interface KaicutProject {
  schemaVersion: 2
  appVersion: string
  projectId: string
  name: string
  createdAt: string
  updatedAt: string
  timebase: {
    fpsNumerator: number
    fpsDenominator: number
  }
  mediaRefs: PersistedMediaAsset[]
  tracks: TimelineTrack[]
}

export interface ProjectState {
  project: KaicutProject
  media: MediaAsset[]
  isDirty: boolean
  statusMessage: string
  projectPath: string | null
  busy: boolean
  selectedMediaId: string | null
  selectedClipId: string | null
  selectedClipIds: string[]
  previewMode: 'source' | 'timeline'
  playheadFrame: number
  revision: number
  canUndo: boolean
  canRedo: boolean
  recentProjects: RecentProject[]
}

export interface NativeSession {
  /** Transport-only hint on explicit native open/reload; never persisted. */
  resetEditingView?: boolean
  project: KaicutProject
  path: string | null
  dirty: boolean
  status: string
  revision?: number
  canUndo?: boolean
  canRedo?: boolean
}

export interface MediaLinkStatus {
  id: string
  state: Exclude<MediaLinkState, 'unknown'>
  reason?: string
}

export interface MediaProbeResult {
  mediaId: string
  state: Exclude<MediaProbeState, 'idle' | 'probing'>
  durationMs?: number
  width?: number
  height?: number
  fpsNumerator?: number
  fpsDenominator?: number
  audioSampleRate?: number
  audioChannels?: number
  rotation?: number
  error?: string
}

export interface RecoveryStatus {
  available: boolean
  projectId?: string
  projectName?: string
  savedAt?: string
}

export interface RecentProject {
  path: string
  name: string
  updatedAt: string
}
