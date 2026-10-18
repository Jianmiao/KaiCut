import { useEffect, useMemo, useRef, useState, type CSSProperties, type PointerEvent as ReactPointerEvent } from 'react'
import { useShallow } from 'zustand/react/shallow'
import { Copy, Eye, EyeOff, Film, Gauge, Lock, Magnet, Music2, Redo2, Scissors, SlidersHorizontal, Target, Trash2, Undo2, Unlock, Video, Volume2, VolumeX, ZoomIn, ZoomOut } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Slider } from '@/components/ui/slider'
import { isDesktop, useProjectStore } from '@/stores/project-store'
import { frameFromOffset, moveTimelineClips, primaryVideoSnapTargets, snapTimelineFrame, timelineEnd, trimClip } from '@/lib/timeline-view'
import { maxTimelineFrames } from '@/lib/kaicut-format'
import { formatMediaDuration } from '@/lib/media-display'
import type { TimelineClip, TimelineEdit, TimelineTrack } from '@/types/project'
import '@/styles/timeline-layout.css'

function TimelineTool({ label, disabled = false, pressed, onClick, children }: { label: string; disabled?: boolean; pressed?: boolean; onClick?: () => void; children: React.ReactNode }) {
  return <span className="timeline-tool-tip" data-tooltip={label} title={label}>
    <Button variant="ghost" size="icon" disabled={disabled} aria-label={label} aria-pressed={pressed} onClick={onClick}>{children}</Button>
  </span>
}

function AudioMixerPopover({ track, clip, fps }: { track: TimelineTrack; clip?: TimelineClip; fps: number }) {
  const { busy, editTimeline } = useProjectStore(useShallow((state) => ({ busy: state.busy, editTimeline: state.editTimeline })))
  const [volume, setVolume] = useState(track.volume)
  const [fadeInFrames, setFadeInFrames] = useState(clip?.fadeInFrames ?? 0)
  const [fadeOutFrames, setFadeOutFrames] = useState(clip?.fadeOutFrames ?? 0)
  const disabled = !isDesktop || busy || track.locked
  const commitVolume = async () => {
    if (disabled || volume === track.volume) return
    const requested = volume
    await editTimeline({ type: 'trackVolume', trackId: track.id, volume: requested })
    const updated = useProjectStore.getState().project.tracks.find((item) => item.id === track.id)?.volume
    if (updated !== requested) setVolume(updated ?? track.volume)
  }
  const commitFades = async () => {
    if (disabled || !clip || (fadeInFrames === clip.fadeInFrames && fadeOutFrames === clip.fadeOutFrames)) return
    const requestedIn = fadeInFrames
    const requestedOut = fadeOutFrames
    await editTimeline({ type: 'clipFade', clipId: clip.id, fadeInFrames: requestedIn, fadeOutFrames: requestedOut })
    const updated = useProjectStore.getState().project.tracks.flatMap((item) => item.clips).find((item) => item.id === clip.id)
    if (!updated || updated.fadeInFrames !== requestedIn || updated.fadeOutFrames !== requestedOut) {
      setFadeInFrames(updated?.fadeInFrames ?? clip.fadeInFrames ?? 0)
      setFadeOutFrames(updated?.fadeOutFrames ?? clip.fadeOutFrames ?? 0)
    }
  }
  const durationSeconds = (frames: number) => `${(frames / fps).toFixed(1)} s`
  return <div className="track-mixer-popover" role="dialog" aria-label={`${track.name}混音器`}>
    <div className="track-mixer-title"><span>{track.name} · 混音</span><span className="track-mixer-state">{track.muted ? '已静音' : '启用'}</span></div>
    <label className="track-mixer-field">
      <span>音量</span><span className="track-mixer-value">{Math.round(volume * 100)}%</span>
      <input type="range" min="0" max="1" step="0.01" value={volume} disabled={disabled} aria-label={`${track.name}音量`} onChange={(event) => setVolume(Number(event.target.value))} onPointerUp={() => void commitVolume()} onKeyUp={() => void commitVolume()} onBlur={() => void commitVolume()} />
    </label>
    <div className="track-mixer-fades">
      <label><span>淡入</span><output>{clip ? durationSeconds(fadeInFrames) : '—'}</output><input type="range" min="0" max={clip ? Math.max(0, clip.durationFrames - fadeOutFrames) : 1} step="1" value={fadeInFrames} disabled={disabled || !clip} aria-label={`${track.name}淡入时长`} onChange={(event) => setFadeInFrames(Number(event.target.value))} onPointerUp={() => void commitFades()} onKeyUp={() => void commitFades()} onBlur={() => void commitFades()} /></label>
      <label><span>淡出</span><output>{clip ? durationSeconds(fadeOutFrames) : '—'}</output><input type="range" min="0" max={clip ? Math.max(0, clip.durationFrames - fadeInFrames) : 1} step="1" value={fadeOutFrames} disabled={disabled || !clip} aria-label={`${track.name}淡出时长`} onChange={(event) => setFadeOutFrames(Number(event.target.value))} onPointerUp={() => void commitFades()} onKeyUp={() => void commitFades()} onBlur={() => void commitFades()} /></label>
    </div>
    <p className="track-mixer-note">{clip ? `当前片段 · ${clip.durationFrames} 帧` : '选择音频片段后调整淡入淡出'}</p>
  </div>
}

export function Timeline() {
  const { project, media, busy, selectedClipId, selectedClipIds, playheadFrame, canUndo, canRedo, selectClip, selectClips, toggleClipSelection, setPlayheadFrame, editTimeline, undoTimeline, redoTimeline, ensureWaveform } = useProjectStore(useShallow((state) => ({
    project: state.project, media: state.media, busy: state.busy, selectedClipId: state.selectedClipId, selectedClipIds: state.selectedClipIds,
    playheadFrame: state.playheadFrame, canUndo: state.canUndo, canRedo: state.canRedo, selectClip: state.selectClip, selectClips: state.selectClips,
    toggleClipSelection: state.toggleClipSelection, setPlayheadFrame: state.setPlayheadFrame, editTimeline: state.editTimeline,
    undoTimeline: state.undoTimeline, redoTimeline: state.redoTimeline, ensureWaveform: state.ensureWaveform,
  })))
  const { tracks, timebase, name } = project
  const [zoom, setZoom] = useState(32)
  const [snapping, setSnapping] = useState(true)
  const [mainRailSnapping, setMainRailSnapping] = useState(true)
  const [mixerTrackId, setMixerTrackId] = useState<string | null>(null)
  const [draft, setDraft] = useState<TimelineClip | null>(null)
  const [draftGroup, setDraftGroup] = useState<TimelineClip[] | null>(null)
  const [focusedClipId, setFocusedClipId] = useState<string | null>(null)
  const cancelGesture = useRef<(() => void) | null>(null)
  const draggedClipId = useRef<string | null>(null)
  const scrollRef = useRef<HTMLDivElement>(null)
  const fps = timebase.fpsNumerator / timebase.fpsDenominator
  const { clipById, trackById, end } = useMemo(() => {
    const clipById = new Map<string, { clip: TimelineClip; track: TimelineTrack }>()
    const trackById = new Map(project.tracks.map((track) => [track.id, track]))
    for (const track of project.tracks) for (const clip of track.clips) clipById.set(clip.id, { clip, track })
    return { clipById, trackById, end: timelineEnd(project.tracks) }
  }, [project])
  const mediaById = useMemo(() => new Map(media.map((asset) => [asset.id, asset])), [media])
  const selectedSet = useMemo(() => new Set(selectedClipIds), [selectedClipIds])
  const selectedEntry = selectedClipId ? clipById.get(selectedClipId) : undefined
  const selectedTrack = selectedEntry?.track
  const selected = selectedEntry?.clip
  const canEdit = isDesktop && !busy && !!selected && selectedClipIds.length === 1 && !selectedTrack?.locked
  const editableSelection = useMemo(() => selectedClipIds.length > 0 && selectedClipIds.every((id) => {
    const entry = clipById.get(id)
    return !!entry && !entry.track.locked
  }), [clipById, selectedClipIds])
  const canEditSelection = isDesktop && !busy && editableSelection
  const canSplit = canEdit && playheadFrame > selected!.startFrame && playheadFrame < selected!.startFrame + selected!.durationFrames
  // Width can span long projects; render only the visible ruler/clip window.
  const [viewport, setViewport] = useState({ left: 0, width: 1000 })
  useEffect(() => {
    const element = scrollRef.current
    if (!element || typeof ResizeObserver === 'undefined') return
    const update = () => setViewport({ left: element.scrollLeft, width: element.clientWidth })
    const observer = new ResizeObserver(update); observer.observe(element); update()
    return () => observer.disconnect()
  }, [])
  useEffect(() => () => { cancelGesture.current?.() }, [project.projectId])
  useEffect(() => {
    const ids = new Set(project.tracks.flatMap((track) => track.kind === 'audio' ? track.clips.map((clip) => clip.mediaId) : []))
    ids.forEach(ensureWaveform)
  }, [ensureWaveform, project.tracks])
  const canvasWidth = Math.max(viewport.width - 96, Math.ceil(Math.max(30, (end + fps * 5) / fps) * zoom))
  const visibleStart = Math.max(0, viewport.left / zoom - 2)
  const visibleEnd = (viewport.left + viewport.width) / zoom + 2
  const renderedTracks = useMemo(() => {
    const drafts = draftGroup ?? (draft ? [draft] : [])
    const draftsByTrack = new Map<string, TimelineClip[]>()
    const draftIds = new Set<string>()
    for (const item of drafts) {
      draftIds.add(item.id)
      const lane = draftsByTrack.get(item.trackId) ?? []
      lane.push(item); draftsByTrack.set(item.trackId, lane)
    }
    const inViewport = (clip: TimelineClip) => clip.startFrame / fps < visibleEnd && (clip.startFrame + clip.durationFrames) / fps > visibleStart
    return project.tracks.map((track) => ({
      track,
      clips: [
        ...track.clips.filter((clip) => !draftIds.has(clip.id) && (inViewport(clip) || clip.id === focusedClipId)),
        ...(draftsByTrack.get(track.id) ?? []).filter((clip) => inViewport(clip) || clip.id === draft?.id || clip.id === focusedClipId),
      ],
    }))
  }, [project, draft, draftGroup, focusedClipId, fps, visibleStart, visibleEnd])
  const tickStep = zoom >= 16 ? 1 : 5
  const ticks = Array.from({ length: Math.min(1000, Math.ceil((visibleEnd - visibleStart) / tickStep) + 1) }, (_, index) => (Math.floor(visibleStart / tickStep) + index) * tickStep)
  const selectedRef = useRef({ selected, selectedClipIds, canEdit, canEditSelection, canSplit, playheadFrame, busy, canUndo, canRedo })
  useEffect(() => { selectedRef.current = { selected, selectedClipIds, canEdit, canEditSelection, canSplit, playheadFrame, busy, canUndo, canRedo } })
  useEffect(() => {
    const keydown = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.isComposing || event.altKey || document.querySelector('[aria-modal="true"], [role="menu"], [role="listbox"]')) return
      const element = event.target
      if (element instanceof HTMLElement && (element.isContentEditable || element.closest('input, textarea, select, [role="spinbutton"]'))) return
      const state = selectedRef.current
      if (state.busy) return
      const mod = /Mac/.test(navigator.platform) ? event.metaKey : event.ctrlKey
      const key = event.key.toLowerCase()
      if (mod && key === 'z' && isDesktop) {
        event.preventDefault()
        if (event.shiftKey ? state.canRedo : state.canUndo) void (event.shiftKey ? redoTimeline() : undoTimeline())
      } else if (mod && key === 'b' && state.canSplit) {
        event.preventDefault(); void editTimeline({ type: 'split', clipId: state.selected!.id, frame: state.playheadFrame })
      } else if (event.shiftKey && !mod && (key === 'delete' || key === 'backspace') && state.canEditSelection) {
        event.preventDefault(); void editTimeline({ type: 'rippleDelete', clipIds: state.selectedClipIds })
      } else if (!mod && (key === 'delete' || key === 'backspace') && state.canEditSelection) {
        event.preventDefault(); void editTimeline({ type: 'delete', clipIds: state.selectedClipIds })
      } else if (!mod && (key === 'arrowleft' || key === 'arrowright') && !(element instanceof HTMLElement && element.closest('button,[role="separator"]'))) {
        event.preventDefault(); setPlayheadFrame(Math.max(0, state.playheadFrame + (key === 'arrowleft' ? -1 : 1)))
      }
    }
    window.addEventListener('keydown', keydown)
    return () => window.removeEventListener('keydown', keydown)
  }, [editTimeline, undoTimeline, redoTimeline, setPlayheadFrame])

  const startPlayhead = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.button !== 0 || event.target instanceof Element && event.target.closest('.timeline-clip')) return
    const rect = event.currentTarget.getBoundingClientRect()
    const update = (x: number) => setPlayheadFrame(frameFromOffset(x - rect.left, zoom, fps))
    update(event.clientX)
    event.currentTarget.setPointerCapture(event.pointerId)
  }
  const movePlayhead = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.currentTarget.hasPointerCapture(event.pointerId)) setPlayheadFrame(frameFromOffset(event.clientX - event.currentTarget.getBoundingClientRect().left, zoom, fps))
  }
  const startGesture = (event: ReactPointerEvent<HTMLElement>, clip: TimelineClip, edge?: 'left' | 'right') => {
    if (event.button !== 0) return
    event.stopPropagation()
    draggedClipId.current = null
    // Modified clicks only adjust selection; they must never move an unselected clip.
    if (event.shiftKey || event.ctrlKey || event.metaKey) return
    if (!(selectedClipIds.length > 1 && selectedSet.has(clip.id))) selectClip(clip.id)
    const activeSelection = useProjectStore.getState().selectedClipIds
    const activeSelectionSet = new Set(activeSelection)
    const selectedGroup = activeSelection.length > 1 && activeSelectionSet.has(clip.id)
      ? tracks.flatMap((track) => track.clips).filter((item) => activeSelectionSet.has(item.id))
      : []
    const groupKinds = new Set(selectedGroup.map((item) => trackById.get(item.trackId)?.kind))
    const groupDrag = !edge && selectedGroup.length > 1 && groupKinds.size === 1
    if (activeSelection.length > 1 && !groupDrag) return
    if (!isDesktop || busy || trackById.get(clip.trackId)?.locked) return
    if (selectedGroup.some((item) => trackById.get(item.trackId)?.locked)) return
    cancelGesture.current?.()
    const initial = useProjectStore.getState()
    const startX = event.clientX
    const startY = event.clientY
    const scrollLeft = scrollRef.current?.scrollLeft ?? 0
    let latest = clip
    let latestGroup = selectedGroup
    let changed = false
    let gestureStarted = false
    const movingClips = groupDrag ? selectedGroup : [clip]
    const movingIds = movingClips.map((item) => item.id)
    const movingSet = new Set(movingIds)
    const targets = [0, playheadFrame, ...tracks.flatMap((track) => track.clips.filter((item) => !movingSet.has(item.id)).flatMap((item) => [item.startFrame, item.startFrame + item.durationFrames]))]
    const cleanup = () => {
      window.removeEventListener('pointermove', move)
      window.removeEventListener('pointerup', finish)
      window.removeEventListener('pointercancel', cancel)
      window.removeEventListener('keydown', escape)
      cancelGesture.current = null
      setDraft(null)
      setDraftGroup(null)
    }
    const cancel = () => cleanup()
    const escape = (key: KeyboardEvent) => { if (key.key === 'Escape') { key.preventDefault(); cleanup() } }
    const move = (pointer: PointerEvent) => {
      const pixels = pointer.clientX - startX + (scrollRef.current?.scrollLeft ?? 0) - scrollLeft
      if (!gestureStarted && Math.hypot(pixels, pointer.clientY - startY) < 3) return
      gestureStarted = true
      draggedClipId.current = clip.id
      const delta = Math.round(pixels / zoom * fps)
      if (edge) {
        const oldEdge = edge === 'left' ? clip.startFrame : clip.startFrame + clip.durationFrames
        const newEdge = snapping && !pointer.altKey ? snapTimelineFrame(oldEdge + delta, targets, fps * 6 / zoom) : oldEdge + delta
        latest = trimClip(clip, edge, newEdge - oldEdge)
      } else {
        let startFrame = Math.max(0, clip.startFrame + delta)
        const lane = document.elementFromPoint(pointer.clientX, pointer.clientY)?.closest<HTMLElement>('[data-track-id]')
        const target = lane?.dataset.trackId ? trackById.get(lane.dataset.trackId) : undefined
        const original = trackById.get(clip.trackId)!
        const mainRailTargets = target?.id === 'video-1' && original.kind === 'video' && mainRailSnapping
          ? primaryVideoSnapTargets(tracks, movingIds)
          : []
        if (!pointer.altKey && (snapping || mainRailTargets.length > 0)) {
          const options = [...(snapping ? targets : []), ...mainRailTargets]
          const aligned = [...options, ...options.map((target) => target - clip.durationFrames)]
          startFrame = snapTimelineFrame(startFrame, aligned, fps * 6 / zoom)
        }
        const targetTrackId = target && target.kind === original.kind && !target.locked ? target.id : clip.trackId
        latestGroup = moveTimelineClips(tracks, movingClips, clip, targetTrackId, startFrame, maxTimelineFrames(timebase))
        latest = latestGroup.find((item) => item.id === clip.id)!
      }
      changed = latest.startFrame !== clip.startFrame || latest.trackId !== clip.trackId || latest.durationFrames !== clip.durationFrames || latest.sourceInFrame !== clip.sourceInFrame
      setDraft(latest)
      if (groupDrag) setDraftGroup(latestGroup)
    }
    const finish = () => {
      cleanup()
      const state = useProjectStore.getState()
      if (!changed || state.project.projectId !== initial.project.projectId || state.revision !== initial.revision || state.busy) return
      const edit: TimelineEdit = groupDrag
        ? { type: 'moveMany', moves: latestGroup.map((item) => ({ clipId: item.id, trackId: item.trackId, startFrame: item.startFrame })) }
        : edge
          ? { type: 'trim', clipId: clip.id, startFrame: latest.startFrame, sourceInFrame: latest.sourceInFrame, durationFrames: latest.durationFrames }
          : { type: 'move', clipId: clip.id, trackId: latest.trackId, startFrame: latest.startFrame }
      void editTimeline(edit)
    }
    event.preventDefault()
    cancelGesture.current = cancel
    window.addEventListener('pointermove', move)
    window.addEventListener('pointerup', finish, { once: true })
    window.addEventListener('pointercancel', cancel, { once: true })
    window.addEventListener('keydown', escape)
  }
  const submit = (edit: TimelineEdit) => { void editTimeline(edit) }
  const selectFromInteraction = (clip: TimelineClip, event: React.MouseEvent<HTMLButtonElement>) => {
    if (draggedClipId.current === clip.id) { draggedClipId.current = null; return }
    const additive = event.ctrlKey || event.metaKey
    if (event.shiftKey) {
      const track = tracks.find((item) => item.id === clip.trackId)
      const anchor = selectedClipId ? track?.clips.find((item) => item.id === selectedClipId) : undefined
      if (track && anchor) {
        const ordered = [...track.clips].sort((left, right) => left.startFrame - right.startFrame)
        const start = ordered.findIndex((item) => item.id === anchor.id)
        const end = ordered.findIndex((item) => item.id === clip.id)
        const range = ordered.slice(Math.min(start, end), Math.max(start, end) + 1).map((item) => item.id)
        selectClips(additive ? [...selectedClipIds, ...range] : range, clip.id)
        return
      }
    }
    if (additive) toggleClipSelection(clip.id)
    else selectClip(clip.id)
  }
  return <section className="timeline-panel timeline-workspace" aria-label="时间线"
    onFocusCapture={(event) => setFocusedClipId(event.target.closest<HTMLElement>('[data-clip-id]')?.dataset.clipId ?? null)}
    onBlurCapture={(event) => setFocusedClipId(event.relatedTarget instanceof Element ? event.relatedTarget.closest<HTMLElement>('[data-clip-id]')?.dataset.clipId ?? null : null)}>
    <div className="timeline-edit-tools" aria-label="时间线工具">
      <div className="timeline-edit-group" role="group" aria-label="历史记录">
        <TimelineTool label="撤销（Ctrl+Z）" disabled={!isDesktop || busy || !canUndo} onClick={() => void undoTimeline()}><Undo2 size={16} /></TimelineTool>
        <TimelineTool label="重做（Ctrl+Shift+Z）" disabled={!isDesktop || busy || !canRedo} onClick={() => void redoTimeline()}><Redo2 size={16} /></TimelineTool>
      </div><span className="timeline-tool-divider" aria-hidden="true" />
      <div className="timeline-edit-group" role="group" aria-label="片段操作">
        <TimelineTool label="分割（Ctrl+B）" disabled={!canSplit} onClick={() => submit({ type: 'split', clipId: selected!.id, frame: playheadFrame })}><Scissors size={16} /></TimelineTool>
        <TimelineTool label={`删除所选片段（Delete）${selectedClipIds.length > 1 ? ` · ${selectedClipIds.length} 个` : ''}`} disabled={!canEditSelection} onClick={() => submit({ type: 'delete', clipIds: selectedClipIds })}><Trash2 size={16} /></TimelineTool>
        <TimelineTool label={`波纹删除（Shift+Delete）${selectedClipIds.length > 1 ? ` · ${selectedClipIds.length} 个` : ''}`} disabled={!canEditSelection} onClick={() => submit({ type: 'rippleDelete', clipIds: selectedClipIds })}><Trash2 size={16} /></TimelineTool>
        <TimelineTool label="创建副本（追加到轨道末尾）" disabled={!canEdit} onClick={() => submit({ type: 'duplicate', clipId: selected!.id })}><Copy size={16} /></TimelineTool>
      </div><span className="timeline-tool-divider" aria-hidden="true" />
      <span className="timeline-selection-count" aria-live="polite">{selectedClipIds.length > 1 ? `已选 ${selectedClipIds.length} 个片段` : ''}</span>
      <div className="timeline-edit-group" role="group" aria-label="片段调整">
        <TimelineTool label={selectedTrack?.kind === 'audio' ? '打开音频混音器' : '选择音频轨道后打开混音器'} disabled={!isDesktop || busy || selectedTrack?.kind !== 'audio'} pressed={selectedTrack?.kind === 'audio' && mixerTrackId === selectedTrack.id} onClick={() => selectedTrack?.kind === 'audio' && setMixerTrackId(mixerTrackId === selectedTrack.id ? null : selectedTrack.id)}><SlidersHorizontal size={16} /></TimelineTool>
        <TimelineTool label="速度尚未开放" disabled><Gauge size={16} /></TimelineTool>
      </div>
      <div className="timeline-display-tools" role="group" aria-label="时间线显示">
        <TimelineTool label="吸附（拖动时按 Alt 临时关闭）" pressed={snapping} onClick={() => setSnapping(!snapping)}><Magnet size={16} /></TimelineTool>
        <TimelineTool label="主轨磁吸（video-1，拖到主轨片段边界）" pressed={mainRailSnapping} onClick={() => setMainRailSnapping(!mainRailSnapping)}><Target size={16} /></TimelineTool>
        <TimelineTool label="缩小时间线" disabled={zoom <= 4} onClick={() => setZoom(Math.max(4, zoom / 2))}><ZoomOut size={16} /></TimelineTool>
        <Slider className="timeline-zoom" aria-label="时间线缩放" min={4} max={128} step={4} value={zoom} onChange={setZoom} />
        <TimelineTool label="放大时间线" disabled={zoom >= 128} onClick={() => setZoom(Math.min(128, zoom * 2))}><ZoomIn size={16} /></TimelineTool>
      </div>
    </div>
    <div ref={scrollRef} className="timeline-scroll" onScroll={(event) => setViewport({ left: event.currentTarget.scrollLeft, width: event.currentTarget.clientWidth })}>
      <div className="timeline-content" style={{ '--timeline-canvas-width': `${canvasWidth}px` } as CSSProperties}>
        <div className="timeline-ruler-row"><span className="track-label">轨道</span><div className="timeline-ruler" role="slider" aria-label="时间线播放头（仅定位）" aria-valuemin={0} aria-valuemax={Math.max(end, playheadFrame, Math.round(canvasWidth / zoom * fps))} aria-valuenow={playheadFrame} aria-valuetext={`${playheadFrame} 帧`} tabIndex={0} onPointerDown={startPlayhead} onPointerMove={movePlayhead} onPointerUp={(event) => { if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId) }} onKeyDown={(event) => { if (event.key === 'Home') { event.preventDefault(); setPlayheadFrame(0) } if (event.key === 'End') { event.preventDefault(); setPlayheadFrame(end) } }}>
          {ticks.map((second) => <span className="ruler-tick" style={{ left: second * zoom }} data-major={second % (tickStep * 5) === 0} key={second}>{second % (tickStep * 5) === 0 && <span className="ruler-label">{Math.floor(second / 60).toString().padStart(2, '0')}:{(second % 60).toString().padStart(2, '0')}</span>}</span>)}
          <span className="ruler-playhead" style={{ left: playheadFrame / fps * zoom }} />
        </div></div>
        <div className="timeline-tracks">
          {renderedTracks.map(({ track, clips }) => <div className="timeline-track" key={track.id} role="group" aria-label={`${track.name}，${track.clips.length} 个片段${track.locked ? '，已锁定' : ''}`}>
            <div className="track-control-stack"><div className="track-control-row"><div className={`track-icon track-icon-${track.kind}`} title={track.name}>{track.kind === 'video' ? <Video size={16} /> : <Music2 size={16} />}</div><div className="track-control-actions">
              <TimelineTool label={`${track.locked ? '解锁' : '锁定'}${track.name}`} disabled={!isDesktop || busy} pressed={track.locked} onClick={() => submit({ type: 'trackState', trackId: track.id, locked: !track.locked, muted: track.muted })}>{track.locked ? <Lock size={14} /> : <Unlock size={14} />}</TimelineTool>
              <TimelineTool label={`${track.muted ? '启用' : '停用'}${track.name}`} disabled={!isDesktop || busy || track.locked} pressed={track.muted} onClick={() => submit({ type: 'trackState', trackId: track.id, locked: track.locked, muted: !track.muted })}>{track.kind === 'video' ? track.muted ? <EyeOff size={14} /> : <Eye size={14} /> : track.muted ? <VolumeX size={14} /> : <Volume2 size={14} />}</TimelineTool>
              {track.kind === 'audio' && <TimelineTool label={`${mixerTrackId === track.id ? '关闭' : '打开'}${track.name}混音器`} disabled={!isDesktop || busy || track.locked} pressed={mixerTrackId === track.id} onClick={() => setMixerTrackId(mixerTrackId === track.id ? null : track.id)}><SlidersHorizontal size={14} /></TimelineTool>}
            </div></div>{mixerTrackId === track.id && track.kind === 'audio' && <AudioMixerPopover key={`${track.id}:${track.volume}:${selectedClipId ?? ''}:${selected?.trackId === track.id ? selected.fadeInFrames ?? 0 : 0}:${selected?.trackId === track.id ? selected.fadeOutFrames ?? 0 : 0}`} track={track} clip={selected?.trackId === track.id ? selected : undefined} fps={fps} />}</div>
            <div className="track-row" data-track-id={track.id} onPointerDown={startPlayhead} onPointerMove={movePlayhead}>
              {clips.map((clip) => {
                const asset = mediaById.get(clip.mediaId)
                const image = asset?.linkState === 'online' && asset.thumbnail?.state === 'ready' ? asset.thumbnail.dataUrl : undefined
                return <div className={`timeline-clip timeline-clip-${track.kind}`} data-clip-id={clip.id} data-selected={selectedSet.has(clip.id)} data-primary={clip.id === selectedClipId} data-muted={track.muted} key={clip.id} style={{ left: clip.startFrame / fps * zoom, width: Math.max(2, clip.durationFrames / fps * zoom) }}>
                  <button type="button" className="timeline-clip-body" aria-label={`${asset?.name ?? clip.mediaId}，起点 ${clip.startFrame} 帧，时长 ${clip.durationFrames} 帧`} aria-pressed={selectedSet.has(clip.id)} onClick={(event) => selectFromInteraction(clip, event)} onPointerDown={(event) => startGesture(event, clip)}>
                    {image && <img src={image} alt="" draggable={false} />}{asset?.waveform?.state === 'ready' && asset.waveform.dataUrl && <img className="timeline-waveform" src={asset.waveform.dataUrl} alt="" draggable={false} />}<span>{asset?.name ?? '素材离线'}</span>
                  </button>
                  {clip.id === selectedClipId && selectedClipIds.length === 1 && !track.locked && <><span className="clip-trim-handle clip-trim-left" aria-hidden="true" onPointerDown={(event) => startGesture(event, clip, 'left')} /><span className="clip-trim-handle clip-trim-right" aria-hidden="true" onPointerDown={(event) => startGesture(event, clip, 'right')} /></>}
                </div>
              })}
            </div>
          </div>)}
        </div>
        {end === 0 && <div className="timeline-empty-space"><div className="timeline-empty-guidance"><Film size={16} /><div><p>时间线为空</p><span>{isDesktop ? '双击素材，或在检查器中添加到时间线' : '桌面版可添加与编辑片段'}</span></div></div></div>}
        <span className="timeline-playhead-line" style={{ left: 96 + playheadFrame / fps * zoom }} aria-hidden="true" />
      </div>
    </div>
    <div className="timeline-footer"><div className="timeline-sequence-caption"><Film size={12} /><span title={name}>{name}</span></div><span className="timeline-sequence-metadata">{playheadFrame} 帧<span aria-hidden="true">·</span>{Number(fps.toFixed(3))} fps<span>仅编辑定位</span></span><span className="sequence-duration" aria-label="时间线总时长">{formatMediaDuration(end / fps * 1000)}</span></div>
  </section>
}
