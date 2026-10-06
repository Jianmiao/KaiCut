import type { KaicutProject, MediaAsset } from '@/types/project'

export const CURRENT_SCHEMA_VERSION = 1 as const
export const APP_VERSION = '0.1.0'

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
      { id: 'video-1', name: '视频 1', kind: 'video', locked: false, muted: false, clips: [] },
      { id: 'video-2', name: '视频 2', kind: 'video', locked: false, muted: false, clips: [] },
      { id: 'audio-1', name: '音频 1', kind: 'audio', locked: false, muted: false, clips: [] },
      { id: 'audio-2', name: '音频 2', kind: 'audio', locked: false, muted: false, clips: [] },
    ],
  }
}

export function serializeProject(project: KaicutProject, media: MediaAsset[]) {
  const snapshot: KaicutProject = {
    ...project,
    updatedAt: new Date().toISOString(),
    mediaRefs: media.map(({ objectUrl: _objectUrl, ...asset }) => asset),
  }
  return JSON.stringify(snapshot, null, 2)
}

export function parseProject(raw: string): KaicutProject {
  if (raw.length > 10 * 1024 * 1024) throw new Error('项目文件超过 10 MiB')
  const value: unknown = JSON.parse(raw)
  const invalid = () => { throw new Error('项目数据无效或包含当前版本不支持的字段') }
  const record = (item: unknown): item is Record<string, unknown> => !!item && typeof item === 'object' && !Array.isArray(item)
  const only = (item: Record<string, unknown>, keys: string[]) => Object.keys(item).every((key) => keys.includes(key))
  const text = (item: unknown): item is string => typeof item === 'string' && item.trim().length > 0
  const integer = (item: unknown): item is number => typeof item === 'number' && Number.isSafeInteger(item) && item >= 0
  if (!record(value)) return invalid()
  if (value.schemaVersion !== 1) throw new Error(`不支持的项目版本：${String(value.schemaVersion ?? '未知')}`)
  if (!only(value, ['schemaVersion', 'appVersion', 'projectId', 'name', 'createdAt', 'updatedAt', 'timebase', 'mediaRefs', 'tracks']) ||
    !text(value.name) || value.name.length > 120 || !text(value.projectId) || !text(value.appVersion) ||
    !text(value.createdAt) || !Number.isFinite(Date.parse(value.createdAt)) ||
    !text(value.updatedAt) || !Number.isFinite(Date.parse(value.updatedAt)) ||
    !record(value.timebase) || !only(value.timebase, ['fpsNumerator', 'fpsDenominator']) ||
    !integer(value.timebase.fpsNumerator) || value.timebase.fpsNumerator === 0 ||
    !integer(value.timebase.fpsDenominator) || value.timebase.fpsDenominator === 0 ||
    !Array.isArray(value.mediaRefs) || !Array.isArray(value.tracks)) return invalid()
  const ids = new Set<string>()
  for (const media of value.mediaRefs) {
    if (!record(media) || !only(media, ['id', 'name', 'kind', 'extension', 'size', 'lastModified', 'sourcePath']) ||
      !text(media.id) || ids.has(media.id) || !text(media.name) || !text(media.extension) ||
      !['video', 'audio', 'image'].includes(String(media.kind)) || getMediaKind(media.name) !== media.kind || media.name.split('.').pop()?.toLowerCase() !== media.extension ||
      !integer(media.size) || !integer(media.lastModified) ||
      (media.sourcePath !== undefined && (!text(media.sourcePath) || media.sourcePath.includes(String.fromCharCode(0))))) return invalid()
    ids.add(media.id)
  }
  let videos = 0; let audios = 0
  for (const track of value.tracks) {
    if (!record(track) || !only(track, ['id', 'name', 'kind', 'locked', 'muted', 'clips']) ||
      !text(track.id) || ids.has(track.id) || !text(track.name) || !['video', 'audio'].includes(String(track.kind)) ||
      typeof track.locked !== 'boolean' || typeof track.muted !== 'boolean' || !Array.isArray(track.clips)) return invalid()
    if (track.clips.length > 0) throw new Error('当前骨架不支持含剪辑片段的项目，原文件未修改')
    if (track.kind === 'video') videos++; else audios++
    ids.add(track.id)
  }
  if (videos > 2 || audios > 2 || value.tracks.length === 0) return invalid()
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
    // Thumbnails are placeholders until the media sidecar is connected.
  }
}
