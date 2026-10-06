export type MediaKind = 'video' | 'audio' | 'image'

export interface MediaAsset {
  id: string
  name: string
  kind: MediaKind
  extension: string
  size: number
  lastModified: number
  sourcePath?: string
  objectUrl?: string
}

export interface TimelineClip {
  id: string
  mediaId: string
  trackId: string
  startFrame: number
  durationFrames: number
}

export interface TimelineTrack {
  id: string
  name: string
  kind: 'video' | 'audio'
  locked: boolean
  muted: boolean
  clips: TimelineClip[]
}

export interface KaicutProject {
  schemaVersion: 1
  appVersion: string
  projectId: string
  name: string
  createdAt: string
  updatedAt: string
  timebase: {
    fpsNumerator: number
    fpsDenominator: number
  }
  mediaRefs: Omit<MediaAsset, 'objectUrl'>[]
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
}

export interface NativeSession {
  project: KaicutProject
  path: string | null
  dirty: boolean
  status: string
}
