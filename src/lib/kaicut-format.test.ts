import { describe, expect, it } from 'vitest'
import { createEmptyProject, getMediaKind, parseProject, serializeProject } from '@/lib/kaicut-format'

describe('kaicut project format', () => {
  it('round-trips a single-file .kaicut JSON document', () => {
    const project = createEmptyProject('测试项目')
    const parsed = parseProject(serializeProject(project, []))

    expect(parsed.name).toBe('测试项目')
    expect(parsed.schemaVersion).toBe(1)
    expect(parsed.tracks).toHaveLength(4)
  })

  it('rejects unsupported schema versions', () => {
    expect(() => parseProject(JSON.stringify({ schemaVersion: 99, name: 'invalid' }))).toThrow('不支持的项目版本')
  })

  it('preserves external media paths without session URLs', () => {
    const project = createEmptyProject('示例')
    const raw = serializeProject(project, [{ id: 'm1', name: '画面.mov', kind: 'video', extension: 'mov', size: 123, lastModified: 1234, sourcePath: 'E:/素材/画面.mov', objectUrl: 'blob:temporary' }])
    expect(raw).not.toContain('blob:')
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
