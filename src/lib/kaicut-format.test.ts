import { describe, expect, it } from 'vitest'
import { createEmptyProject, getMediaKind, parseProject, serializeProject } from '@/lib/kaicut-format'
import type { KaicutProject, TimelineClip } from '@/types/project'

function editedProject(): KaicutProject {
  const project = createEmptyProject('剪辑项目')
  project.mediaRefs.push({ id: 'media-1', name: '画面.mp4', kind: 'video', extension: 'mp4', size: 100, lastModified: 200 })
  project.tracks[0].clips.push({ id: 'clip-1', mediaId: 'media-1', trackId: 'video-1', startFrame: 30, durationFrames: 60, sourceInFrame: 15, sourceDurationFrames: 120, fadeInFrames: 0, fadeOutFrames: 0 })
  return project
}

describe('kaicut project format', () => {
  it('round-trips a single-file .kaicut JSON document', () => {
    const project = createEmptyProject('测试项目')
    const parsed = parseProject(serializeProject(project, []))

    expect(parsed.name).toBe('测试项目')
    expect(parsed.schemaVersion).toBe(2)
    expect(parsed.tracks).toHaveLength(4)
  })

  it('rejects unsupported schema versions', () => {
    expect(() => parseProject(JSON.stringify({ schemaVersion: 99, name: 'invalid' }))).toThrow('不支持的项目版本')
  })

  it('upgrades a v1 empty timeline without losing media or track settings', () => {
    const project = editedProject()
    project.tracks[0].clips = []
    project.tracks[0].locked = true
    const legacy = { ...project, schemaVersion: 1 }
    expect(parseProject(JSON.stringify(legacy))).toEqual(project)
    expect(legacy.schemaVersion).toBe(1)
  })

  it('rejects a v1 project containing clips instead of guessing the missing source offsets', () => {
    expect(() => parseProject(JSON.stringify({ ...editedProject(), schemaVersion: 1 }))).toThrow('无法安全升级')
  })

  it('round-trips source bounds and accepts adjacent clips even when stored out of order', () => {
    const project = editedProject()
    project.tracks[0].clips.unshift({ ...project.tracks[0].clips[0], id: 'clip-2', startFrame: 90 })
    expect(parseProject(serializeProject(project, project.mediaRefs))).toMatchObject({ tracks: project.tracks })
  })

  it('normalizes legacy v2 mixer fields and preserves new audio settings', () => {
    const project = createEmptyProject()
    project.mediaRefs.push({ id: 'audio-1', name: '配乐.wav', kind: 'audio', extension: 'wav', size: 1, lastModified: 1 })
    project.tracks[2].volume = 0.45
    project.tracks[2].clips.push({ id: 'audio-clip', mediaId: 'audio-1', trackId: 'audio-1', startFrame: 0, durationFrames: 90, sourceInFrame: 0, sourceDurationFrames: 90, fadeInFrames: 12, fadeOutFrames: 18 })
    const raw = JSON.parse(serializeProject(project, project.mediaRefs)) as Record<string, unknown>
    const tracks = raw.tracks as Array<Record<string, unknown>>
    delete tracks[0].volume
    const clips = tracks[2].clips as Array<Record<string, unknown>>
    delete clips[0].fadeInFrames
    delete clips[0].fadeOutFrames
    const parsed = parseProject(JSON.stringify(raw))
    expect(parsed.tracks[0].volume).toBe(1)
    expect(parsed.tracks[2].volume).toBe(0.45)
    expect(parsed.tracks[2].clips[0]).toMatchObject({ fadeInFrames: 0, fadeOutFrames: 0 })
  })

  it('rejects invalid mixer ranges before accepting the project', () => {
    const project = createEmptyProject()
    project.mediaRefs.push({ id: 'audio-1', name: '配乐.wav', kind: 'audio', extension: 'wav', size: 1, lastModified: 1 })
    project.tracks[2].clips.push({ id: 'audio-clip', mediaId: 'audio-1', trackId: 'audio-1', startFrame: 0, durationFrames: 90, sourceInFrame: 0, sourceDurationFrames: 90, fadeInFrames: 0, fadeOutFrames: 0 })
    project.tracks[2].volume = 1.1
    expect(() => parseProject(JSON.stringify(project))).toThrow()
    project.tracks[2].volume = 1
    project.tracks[2].clips[0].fadeInFrames = 60
    project.tracks[2].clips[0].fadeOutFrames = 31
    expect(() => parseProject(JSON.stringify(project))).toThrow()
  })

  it.each<Partial<TimelineClip>>([
    { sourceInFrame: -1 }, { sourceInFrame: 0.5 }, { sourceInFrame: 61 },
    { sourceDurationFrames: 0 }, { sourceDurationFrames: 30 }, { sourceDurationFrames: 2_592_001 },
    { durationFrames: 0 }, { durationFrames: 0.5 }, { startFrame: -1 }, { startFrame: Number.MAX_SAFE_INTEGER + 1 },
    { startFrame: 2_592_000 }, { mediaId: 'absent' }, { trackId: 'video-2' }, { id: '' },
  ])('rejects invalid clip bounds or references: %j', (patch) => {
    const project = editedProject()
    Object.assign(project.tracks[0].clips[0], patch)
    expect(() => parseProject(JSON.stringify(project))).toThrow()
  })

  it('rejects overlapping clips, duplicate clip ids, missing source bounds and unknown clip fields', () => {
    for (const patch of [{ id: 'overlap', startFrame: 89 }, { startFrame: 90 }]) {
      const project = editedProject()
      project.tracks[0].clips.push({ ...project.tracks[0].clips[0], ...patch })
      expect(() => parseProject(JSON.stringify(project))).toThrow()
    }
    const project = editedProject()
    const clip = project.tracks[0].clips[0]
    for (const invalid of [{ ...clip, sourceInFrame: undefined }, { ...clip, speed: 2 }]) {
      const raw = { ...project, tracks: [{ ...project.tracks[0], clips: [invalid] }] }
      expect(() => parseProject(JSON.stringify(raw))).toThrow()
    }
  })

  it('matches Rust identifier namespaces and validates text length and controls', () => {
    const project = editedProject()
    project.tracks[0].clips[0].id = 'video-1'
    expect(parseProject(JSON.stringify(project)).tracks[0].clips[0].id).toBe('video-1')
    for (const name of ['x'.repeat(121), 'bad\nname']) {
      expect(() => parseProject(JSON.stringify({ ...project, name }))).toThrow()
    }
  })

  it('enforces video/image versus audio tracks and the two-tracks-per-kind limit', () => {
    const project = editedProject()
    project.tracks[2].clips.push({ ...project.tracks[0].clips[0], id: 'clip-2', trackId: 'audio-1' })
    expect(() => parseProject(JSON.stringify(project))).toThrow()
    project.tracks[2].clips = []
    project.tracks.push({ ...project.tracks[1], id: 'video-3' })
    expect(() => parseProject(JSON.stringify(project))).toThrow()
    project.tracks.pop()
    project.mediaRefs[0] = { ...project.mediaRefs[0], kind: 'image', name: '图片.png', extension: 'png' }
    expect(parseProject(JSON.stringify(project)).tracks[0].clips).toHaveLength(1)
  })

  it('uses the project timebase for the exact 24-hour boundary', () => {
    const project = editedProject()
    project.timebase = { fpsNumerator: 30_000, fpsDenominator: 1001 }
    const limit = Math.floor(86_400 * 30_000 / 1001)
    Object.assign(project.tracks[0].clips[0], { startFrame: limit - 1, durationFrames: 1, sourceInFrame: 0, sourceDurationFrames: 1 })
    expect(parseProject(JSON.stringify(project))).toEqual(project)
    project.tracks[0].clips[0].startFrame += 1
    expect(() => parseProject(JSON.stringify(project))).toThrow()
  })

  it.each([{ fpsNumerator: 1_000_001, fpsDenominator: 5000 }, { fpsNumerator: 30, fpsDenominator: 1_000_001 }, { fpsNumerator: 241, fpsDenominator: 1 }])('rejects unsupported timebases: %j', (timebase) => {
    expect(() => parseProject(JSON.stringify({ ...createEmptyProject(), timebase }))).toThrow()
  })

  it('rejects more than 10000 clips before accepting excessive project work', () => {
    const project = editedProject()
    const sample = project.tracks[0].clips[0]
    project.tracks[0].clips = Array.from({ length: 10_001 }, (_, index) => ({ ...sample, id: `clip-${index}`, startFrame: index, durationFrames: 1 }))
    expect(() => parseProject(JSON.stringify(project))).toThrow()
  })

  it('preserves external media paths without session URLs', () => {
    const project = createEmptyProject('示例')
    const raw = serializeProject(project, [{ id: 'm1', name: '画面.mov', kind: 'video', extension: 'mov', size: 123, lastModified: 1234, sourcePath: 'E:/素材/画面.mov', objectUrl: 'blob:temporary', probe: { state: 'ready', durationMs: 1200, width: 1920, height: 1080 }, thumbnail: { state: 'ready', dataUrl: 'data:image/png;base64,temporary' } }])
    expect(raw).not.toContain('blob:')
    expect(raw).not.toContain('durationMs')
    expect(raw).not.toContain('thumbnail')
    expect(raw).not.toContain('data:image')
    expect(parseProject(raw).mediaRefs[0].sourcePath).toBe('E:/素材/画面.mov')
  })

  it.each([
    { ...createEmptyProject(), timebase: { fpsNumerator: 30, fpsDenominator: 0 } },
    { ...createEmptyProject(), tracks: [null] },
    { ...createEmptyProject(), mediaRefs: [{ id: 'bad' }] },
    { ...createEmptyProject(), unknownFeature: true },
    { ...createEmptyProject(), tracks: [{ ...createEmptyProject().tracks[0], clips: [{ id: 'unsupported' }] }] },
  ])('rejects unsupported or malformed data rather than silently losing it', (project) => {
    expect(() => parseProject(JSON.stringify(project))).toThrow()
  })

  it.each([['film.MP4', 'video'], ['film.mov', 'video'], ['sound.mp3', 'audio'], ['sound.wav', 'audio'], ['art.png', 'image'], ['art.jpg', 'image'], ['art.jpeg', 'image'], ['script.exe', null]])('recognizes %s', (name, expected) => {
    expect(getMediaKind(name!)).toBe(expected)
  })
})
