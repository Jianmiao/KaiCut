import type { KaicutProject, MediaAsset } from '@/types/project'

export const CURRENT_SCHEMA_VERSION = 2 as const
export const APP_VERSION = '0.1.0'

export function maxTimelineFrames(timebase: KaicutProject['timebase']) {
  return Math.floor(24 * 60 * 60 * timebase.fpsNumerator / timebase.fpsDenominator)
}

function createId(prefix: string) {
  const random = typeof crypto !== 'undefined' && 'randomUUID' in crypto
    ? crypto.randomUUID().slice(0, 8)
    : Math.random().toString(36).slice(2, 10)
  return `${prefix}-${random}`
}

export function createEmptyProject(name = '未命名项目'): KaicutProject {
  const now = new Date().toISOString()
  return {
    schemaVersion: CURRENT_SCHEMA_VERSION,
    appVersion: APP_VERSION,
    projectId: createId('project'),
    name,
    createdAt: now,
    updatedAt: now,
    timebase: { fpsNumerator: 30, fpsDenominator: 1 },
    mediaRefs: [],
    tracks: [
      { id: 'video-1', name: '视频 1', kind: 'video', locked: false, muted: false, volume: 1, clips: [] },
      { id: 'video-2', name: '视频 2', kind: 'video', locked: false, muted: false, volume: 1, clips: [] },
      { id: 'audio-1', name: '音频 1', kind: 'audio', locked: false, muted: false, volume: 1, clips: [] },
      { id: 'audio-2', name: '音频 2', kind: 'audio', locked: false, muted: false, volume: 1, clips: [] },
    ],
  }
}

export function serializeProject(project: KaicutProject, media: MediaAsset[]) {
  const snapshot: KaicutProject = {
    ...project,
    updatedAt: new Date().toISOString(),
    tracks: project.tracks.map((track) => ({
      ...track,
      volume: track.volume ?? 1,
      clips: track.clips.map((clip) => ({ ...clip, fadeInFrames: clip.fadeInFrames ?? 0, fadeOutFrames: clip.fadeOutFrames ?? 0 })),
    })),
    mediaRefs: media.map(({ objectUrl: _objectUrl, linkState: _linkState, linkReason: _linkReason, probe: _probe, thumbnail: _thumbnail, waveform: _waveform, ...asset }) => asset),
  }
  return JSON.stringify(snapshot, null, 2)
}

export function parseProject(raw: string): KaicutProject {
  if (new TextEncoder().encode(raw).length > 10 * 1024 * 1024) throw new Error('项目文件超过 10 MiB')
  const value: unknown = JSON.parse(raw)
  const invalid = () => { throw new Error('项目数据无效或包含当前版本不支持的字段') }
  const record = (item: unknown): item is Record<string, unknown> => !!item && typeof item === 'object' && !Array.isArray(item)
  const only = (item: Record<string, unknown>, keys: string[]) => Object.keys(item).every((key) => keys.includes(key))
  const text = (item: unknown, max = 32768): item is string => typeof item === 'string' && item.trim().length > 0 && Array.from(item).length <= max && !/[\p{Cc}]/u.test(item)
  const integer = (item: unknown): item is number => typeof item === 'number' && Number.isSafeInteger(item) && item >= 0
  if (!record(value)) return invalid()
  if (value.schemaVersion !== 1 && value.schemaVersion !== CURRENT_SCHEMA_VERSION) throw new Error(`不支持的项目版本：${String(value.schemaVersion ?? '未知')}`)
  if (!only(value, ['schemaVersion', 'appVersion', 'projectId', 'name', 'createdAt', 'updatedAt', 'timebase', 'mediaRefs', 'tracks']) ||
    !text(value.name, 120) || !text(value.projectId, 128) || !text(value.appVersion, 64) ||
    !text(value.createdAt) || !Number.isFinite(Date.parse(value.createdAt)) ||
    !text(value.updatedAt) || !Number.isFinite(Date.parse(value.updatedAt)) ||
    !record(value.timebase) || !only(value.timebase, ['fpsNumerator', 'fpsDenominator']) ||
    !integer(value.timebase.fpsNumerator) || value.timebase.fpsNumerator === 0 || value.timebase.fpsNumerator > 1_000_000 ||
    !integer(value.timebase.fpsDenominator) || value.timebase.fpsDenominator === 0 || value.timebase.fpsDenominator > 1_000_000 ||
    value.timebase.fpsNumerator / value.timebase.fpsDenominator > 240 ||
    !Array.isArray(value.mediaRefs) || value.mediaRefs.length > 10000 || !Array.isArray(value.tracks)) return invalid()
  const ids = new Set<string>()
  for (const media of value.mediaRefs) {
    if (!record(media) || !only(media, ['id', 'name', 'kind', 'extension', 'size', 'lastModified', 'sourcePath']) ||
      !text(media.id, 128) || ids.has(media.id) || !text(media.name, 1024) || !text(media.extension) ||
      !['video', 'audio', 'image'].includes(String(media.kind)) || getMediaKind(`file.${media.extension}`) !== media.kind ||
      !integer(media.size) || !integer(media.lastModified) ||
      (media.sourcePath !== undefined && (!text(media.sourcePath) || media.sourcePath.includes(String.fromCharCode(0))))) return invalid()
    ids.add(media.id)
  }
  let videos = 0; let audios = 0
  const trackIds = new Set<string>()
  for (const track of value.tracks) {
    if (!record(track) || !only(track, ['id', 'name', 'kind', 'locked', 'muted', 'volume', 'clips']) ||
      !text(track.id, 128) || trackIds.has(track.id) || !text(track.name, 200) || !['video', 'audio'].includes(String(track.kind)) ||
      typeof track.locked !== 'boolean' || typeof track.muted !== 'boolean' ||
      (track.volume !== undefined && (typeof track.volume !== 'number' || !Number.isFinite(track.volume) || track.volume < 0 || track.volume > 1)) ||
      (track.kind === 'video' && track.volume !== undefined && track.volume !== 1) ||
      !Array.isArray(track.clips)) return invalid()
    if (value.schemaVersion === 1 && track.clips.length > 0) throw new Error('旧版项目含有无法安全升级的剪辑片段，原文件未修改')
    if (track.kind === 'video') videos++; else audios++
    trackIds.add(track.id)
    if (track.volume === undefined) track.volume = 1
  }
  if (videos > 2 || audios > 2 || value.tracks.length === 0) return invalid()
  const timebase = value.timebase as KaicutProject['timebase']
  const limit = maxTimelineFrames(timebase)
  if (!Number.isSafeInteger(limit) || limit < 1) return invalid()
  const mediaKinds = new Map((value.mediaRefs as KaicutProject['mediaRefs']).map((media) => [media.id, media.kind]))
  let clipCount = 0
  const clipIds = new Set<string>()
  for (const track of value.tracks as Array<Record<string, unknown>>) {
    const spans: Array<{ start: number; end: number }> = []
    for (const clip of track.clips as unknown[]) {
      if (++clipCount > 10_000) return invalid()
      if (!record(clip) || !only(clip, ['id', 'mediaId', 'trackId', 'startFrame', 'durationFrames', 'sourceInFrame', 'sourceDurationFrames', 'fadeInFrames', 'fadeOutFrames']) ||
        !text(clip.id, 128) || clipIds.has(clip.id) || !text(clip.mediaId, 128) || !mediaKinds.has(clip.mediaId) || clip.trackId !== track.id ||
        !integer(clip.startFrame) || !integer(clip.durationFrames) || clip.durationFrames === 0 ||
        !integer(clip.sourceInFrame) || !integer(clip.sourceDurationFrames) || clip.sourceDurationFrames === 0 ||
        (clip.fadeInFrames !== undefined && (!integer(clip.fadeInFrames) || clip.fadeInFrames > clip.durationFrames)) ||
        (clip.fadeOutFrames !== undefined && (!integer(clip.fadeOutFrames) || clip.fadeOutFrames > clip.durationFrames)) ||
        (Number(clip.fadeInFrames ?? 0) + Number(clip.fadeOutFrames ?? 0) > clip.durationFrames) ||
        clip.sourceDurationFrames > limit || clip.startFrame > limit - clip.durationFrames ||
        clip.sourceInFrame > clip.sourceDurationFrames - clip.durationFrames) return invalid()
      const kind = mediaKinds.get(clip.mediaId)
      if (track.kind === 'audio' ? kind !== 'audio' : kind !== 'video' && kind !== 'image') return invalid()
      clipIds.add(clip.id)
      if (clip.fadeInFrames === undefined) clip.fadeInFrames = 0
      if (clip.fadeOutFrames === undefined) clip.fadeOutFrames = 0
      spans.push({ start: clip.startFrame, end: clip.startFrame + clip.durationFrames })
    }
    spans.sort((left, right) => left.start - right.start)
    if (spans.some((span, index) => index > 0 && spans[index - 1].end > span.start)) return invalid()
  }
  value.schemaVersion = CURRENT_SCHEMA_VERSION
  return value as unknown as KaicutProject
}

export function getMediaKind(fileName: string): MediaAsset['kind'] | null {
  const extension = fileName.split('.').pop()?.toLowerCase()
  if (extension === 'mp4' || extension === 'mov') return 'video'
  if (extension === 'mp3' || extension === 'wav') return 'audio'
  if (extension === 'png' || extension === 'jpg' || extension === 'jpeg') return 'image'
  return null
}

export function toMediaAsset(file: File): MediaAsset {
  const extension = file.name.split('.').pop()?.toLowerCase() ?? ''
  const kind = getMediaKind(file.name)
  if (!kind) throw new Error(`暂不支持的媒体格式：${extension || '未知'}`)
  return {
    id: createId('media'),
    name: file.name,
    kind,
    extension,
    size: file.size,
    lastModified: file.lastModified,
  }
}
