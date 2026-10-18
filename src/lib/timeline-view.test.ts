import { describe, expect, it } from 'vitest'
import { frameFromOffset, moveTimelineClips, primaryVideoSnapTargets, snapTimelineFrame, trimClip } from './timeline-view'
import { createEmptyProject } from './kaicut-format'
const clip = { id: 'c', mediaId: 'm', trackId: 'v', startFrame: 60, durationFrames: 90, sourceInFrame: 30, sourceDurationFrames: 240 }
describe('frame-based timeline gestures', () => {
  it('rounds pixel input to project frames without negative positions', () => {
    expect(frameFromOffset(80, 40, 30000 / 1001)).toBe(60)
    expect(frameFromOffset(-10, 40, 30)).toBe(0)
  })
  it('snaps to nearby edges but does not jump to a distant clip', () => {
    expect(snapTimelineFrame(62, [0, 60, 150], 4)).toBe(60)
    expect(snapTimelineFrame(85, [0, 60, 150], 4)).toBe(85)
  })
  it('uses only video-1 edges for primary-track snapping', () => {
    const tracks = [
      { id: 'video-1', name: '视频 1', kind: 'video' as const, locked: false, muted: false, volume: 1, clips: [clip] },
      { id: 'video-2', name: '视频 2', kind: 'video' as const, locked: false, muted: false, volume: 1, clips: [{ ...clip, id: 'other', trackId: 'video-2', startFrame: 300 }] },
    ]
    expect(primaryVideoSnapTargets(tracks, 'moving')).toEqual([60, 150])
    expect(primaryVideoSnapTargets(tracks, 'c')).toEqual([])
  })
  it('treats the snap threshold and timeline origin as valid boundaries', () => {
    expect(snapTimelineFrame(4, [0], 4)).toBe(0)
    expect(snapTimelineFrame(-3, [0, 60], 4)).toBe(0)
    expect(snapTimelineFrame(124, [120, 150], 4)).toBe(120)
  })
  it('left trimming preserves the far edge and clamps to the available source', () => {
    const shortened = trimClip(clip, 'left', 20)
    expect(shortened.startFrame).toBe(80)
    expect(shortened.sourceInFrame).toBe(50)
    expect(shortened.startFrame + shortened.durationFrames).toBe(150)
    expect(trimClip(clip, 'left', -200).sourceInFrame).toBe(0)
    expect(trimClip(clip, 'left', 200).durationFrames).toBe(1)
  })
  it('right trimming never extends beyond source duration', () => {
    expect(trimClip(clip, 'right', 999).durationFrames).toBe(210)
    expect(trimClip(clip, 'right', -999).durationFrames).toBe(1)
    expect(trimClip(clip, 'right', 12).sourceInFrame).toBe(30)
  })
  it('translates the whole selection between tracks without collapsing track spacing', () => {
    const tracks = createEmptyProject().tracks
    tracks.splice(2, 0, { ...tracks[1], id: 'video-3', name: '视频 3' })
    const anchor = { ...clip, trackId: 'video-1' }
    const other = { ...clip, id: 'other', trackId: 'video-2', startFrame: 150 }
    expect(moveTimelineClips(tracks, [anchor, other], anchor, 'video-2', 90, 1000)).toEqual([
      { ...anchor, trackId: 'video-2', startFrame: 90 },
      { ...other, trackId: 'video-3', startFrame: 180 },
    ])
  })
  it.each(['missing', 'locked'] as const)('keeps every member on its original track if one destination is %s', (reason) => {
    const tracks = createEmptyProject().tracks
    if (reason === 'locked') tracks.splice(2, 0, { ...tracks[1], id: 'video-3', name: '视频 3', locked: true })
    const anchor = { ...clip, trackId: 'video-1' }
    const other = { ...clip, id: 'other', trackId: 'video-2', startFrame: 150 }
    expect(moveTimelineClips(tracks, [anchor, other], anchor, 'video-2', 90, 1000)).toEqual([
      { ...anchor, startFrame: 90 },
      { ...other, startFrame: 180 },
    ])
  })
  it('excludes all selected members from main-rail snap targets', () => {
    const tracks = createEmptyProject().tracks
    tracks[0].clips = [clip, { ...clip, id: 'other', startFrame: 150 }, { ...clip, id: 'stationary', startFrame: 300 }]
    expect(primaryVideoSnapTargets(tracks, ['c', 'other'])).toEqual([300, 390])
  })
})
