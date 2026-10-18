import { useCallback, useEffect, useRef, useState } from 'react'
import { FileAudio, Film, Pause, Play } from 'lucide-react'
import { convertFileSrc } from '@tauri-apps/api/core'
import { Button } from '@/components/ui/button'
import { isDesktop, useProjectStore } from '@/stores/project-store'
import { formatMediaDuration } from '@/lib/media-display'
import { resolveTimelineFrame } from '@/lib/timeline-preview'
import { continuousSource, mediaProjectFrame, syncAdjustment } from '@/lib/preview-clock'
import { useNativePreviewHost } from '@/hooks/use-native-preview-host'
import { useNativeSourceCapability } from '@/hooks/use-native-source-preview'
import { NativeSourcePreview } from './native-source-preview'
import type { KaicutProject, MediaAsset, TimelineClip } from '@/types/project'

export interface TransportRequest {
  action: 'play' | 'pause'
  resolve: () => void
  reject: (error: Error) => void
}

function mediaUrl(asset?: MediaAsset) {
  if (!isDesktop || !asset?.sourcePath || asset.linkState !== 'online') return undefined
  try { return convertFileSrc(asset.sourcePath) } catch { return undefined }
}

interface RegisteredMedia {
  element: HTMLMediaElement
  sync: (frame: number, hold?: boolean, allowPlay?: boolean) => void
  blocked: () => boolean
  needsGate: () => boolean
  prepare: (frame: number, ticket: number) => void
  ready: () => boolean
  playConfirmed: () => boolean
  release: (frame: number) => void
}
let mediaNodeSequence = 0

/** Only active timeline streams are mounted; same-source cuts reuse a decoder. */
function MediaNode({ asset, clip, frame, fps, playing, gain, audio, master, seekEpoch, lane, register, wake, onFailure, onTime, onEnded }: {
  asset: MediaAsset; clip?: TimelineClip; frame: number; fps: number; playing: boolean; gain: number; audio: boolean
  master: boolean; seekEpoch: number; lane: string; register: (lane: string, node: RegisteredMedia | null) => void; wake: () => void
  onFailure: (message: string) => void; onTime?: (time: number) => void; onEnded?: () => void
}) {
  const ref = useRef<HTMLMediaElement | null>(null)
  const alive = useRef(true)
  const pending = useRef<{ epoch: number } | null>(null)
  const generation = useRef(0)
  const confirmedGeneration = useRef(-1)
  const waiting = useRef(false)
  const held = useRef(false)
  const released = useRef(!clip)
  const preparedTicket = useRef(-1)
  const needsSeek = useRef(!!clip)
  const previous = useRef({ clip, seekEpoch, playing })
  const latest = useRef({ clip, frame, fps, playing, gain, master, onFailure })
  useEffect(() => {
    const old = previous.current
    if (old.seekEpoch !== seekEpoch || (old.clip?.id !== clip?.id && !continuousSource(old.clip, clip))) {
      needsSeek.current = !!clip
      released.current = !clip
      generation.current += 1
      pending.current = null
      ref.current?.pause()
    }
    if (old.playing !== playing) { generation.current += 1; pending.current = null; held.current = false; released.current = !clip }
    previous.current = { clip, seekEpoch, playing }
    latest.current = { clip, frame, fps, playing, gain, master, onFailure }
  })
  const sync = useCallback((targetFrame: number, hold?: boolean, allowPlay = false) => {
    const element = ref.current
    if (!element || !alive.current) return
    const current = latest.current
    if (hold !== undefined) held.current = hold && !current.master
    element.volume = Math.max(0, Math.min(1, current.gain))
    const shouldPlay = current.playing && !held.current && (!current.clip || released.current)
    if (current.clip) {
      const desired = (current.clip.sourceInFrame + Math.max(0, targetFrame - current.clip.startFrame)) / current.fps
      const adjustment = syncAdjustment(element.currentTime, desired, { playing: current.playing, master: current.master || held.current || !released.current, seeking: element.seeking, force: needsSeek.current })
      element.playbackRate = shouldPlay ? adjustment.rate : 1
      if (adjustment.seek !== undefined && element.readyState >= 1) {
        try {
          element.currentTime = adjustment.seek
          element.dataset.previewHardSeeks = String(Number(element.dataset.previewHardSeeks ?? 0) + 1)
          needsSeek.current = false
        } catch { /* Retry after metadata without discarding the requested seek. */ }
      } else if (element.readyState >= 1 && !element.seeking && Math.abs(element.currentTime - desired) <= 0.001) needsSeek.current = false
    } else element.playbackRate = 1
    if (!shouldPlay) { if (!element.paused) element.pause(); return }
    if ((!current.clip || allowPlay) && element.paused && !element.ended && !pending.current) {
      const request = { epoch: generation.current }
      pending.current = request
      void element.play().then(() => {
        if (!alive.current || !latest.current.playing || held.current || (latest.current.clip && !released.current)) { element.pause(); element.playbackRate = 1 }
        else if (request.epoch === generation.current) { confirmedGeneration.current = request.epoch; wake() }
        // A stale promise must neither revive a revoked gate nor pause a newer
        // release on the same decoder. Its old rejection is ignored below.
      }).catch((error: unknown) => {
        if (alive.current && latest.current.playing && request.epoch === generation.current && !(error instanceof DOMException && error.name === 'AbortError')) {
          latest.current.onFailure(`无法播放：${asset.name}`)
        }
      }).finally(() => { if (pending.current === request) pending.current = null })
    }
  }, [asset.name, wake])
  useEffect(() => { sync(frame) })
  useEffect(() => {
    alive.current = true
    const element = ref.current
    if (element) register(lane, {
      element, sync, blocked: () => needsSeek.current || waiting.current,
      needsGate: () => !released.current,
      prepare: (target, ticket) => {
        if (preparedTicket.current !== ticket) {
          preparedTicket.current = ticket; released.current = false; held.current = false
          generation.current += 1; pending.current = null; needsSeek.current = !!latest.current.clip
          element.pause(); element.playbackRate = 1
        }
        sync(target)
      },
      ready: () => !needsSeek.current && !element.seeking && element.readyState >= 3 && !element.error,
      playConfirmed: () => confirmedGeneration.current === generation.current,
      release: (target) => { released.current = true; sync(target, false, true) },
    })
    return () => { alive.current = false; generation.current += 1; register(lane, null); element?.pause(); if (element) element.playbackRate = 1 }
  }, [lane, register, sync])
  const props = {
    ref: (element: HTMLMediaElement | null) => {
      if (element && !element.dataset.previewNodeId) { element.dataset.previewNodeId = String(++mediaNodeSequence); element.dataset.previewHardSeeks = '0' }
      ref.current = element
    },
    src: mediaUrl(asset), preload: 'auto', onLoadedMetadata: () => { sync(latest.current.frame); wake() },
    onSeeked: () => { waiting.current = false; sync(latest.current.frame); wake() },
    onWaiting: () => { waiting.current = true; wake() }, onPlaying: () => { waiting.current = false; wake() },
    onCanPlay: () => { waiting.current = false; sync(latest.current.frame); wake() },
    onTimeUpdate: () => onTime?.(ref.current?.currentTime ?? 0), onEnded,
    onError: () => onFailure(`媒体无法播放：${asset.name}，请检查文件或编码格式`),
    'data-preview-media-id': asset.id, 'data-preview-clip-id': clip?.id,
  }
  return audio ? <audio {...props} className="preview-audio" /> : <video {...props} className="preview-image preview-video" playsInline />
}

function PreviewTransport({ project, mode, sourceId, disableNativeHost = false }: { project: KaicutProject; mode: 'source' | 'timeline'; sourceId: string | null; disableNativeHost?: boolean }) {
  const { media, playheadFrame, busy, ensureThumbnail, retryThumbnail } = useProjectStore()
  const fps = project.timebase.fpsNumerator / project.timebase.fpsDenominator
  const timeline = mode === 'timeline'
  const resolved = resolveTimelineFrame(project, media, playheadFrame)
  const asset = timeline ? resolved.visual?.asset : media.find((item) => item.id === sourceId)
  const clip = timeline ? resolved.visual?.clip : undefined
  const audio = timeline ? resolved.audio : []
  const durationFrames = resolved.durationFrames
  const [playing, setPlaying] = useState(false)
  const [sourceTime, setSourceTime] = useState(0)
  const [failure, setFailure] = useState<{ context: string; message: string } | null>(null)
  const [failedImageKey, setFailedImageKey] = useState<string | null>(null)
  const clock = useRef({ frame: playheadFrame, time: 0 })
  const masterProgress = useRef<{ key: string; seconds: number; at: number } | null>(null)
  const ownWrite = useRef(false)
  const [seekEpoch, setSeekEpoch] = useState(0)
  const nodes = useRef(new Map<string, RegisteredMedia>())
  const gate = useRef({ active: false, ticket: 0, target: playheadFrame, startedAt: 0, masterReleased: false, masterInitialSeconds: 0 })
  const evaluate = useRef<(() => void) | null>(null)
  const wake = useCallback(() => evaluate.current?.(), [])
  const stage = useRef<HTMLDivElement>(null)
  const canvas = useRef<HTMLDivElement>(null)
  useNativePreviewHost(canvas, { enabled: !disableNativeHost })
  const register = useCallback((lane: string, node: RegisteredMedia | null) => { if (node) nodes.current.set(lane, node); else nodes.current.delete(lane) }, [])
  const master = audio[0] ?? (resolved.visual?.asset?.kind === 'video' ? resolved.visual : undefined)
  const masterLane = master?.track.kind === 'audio' ? master.track.id : master ? 'visual' : undefined
  const stableSourceKey = `${asset?.kind}:${asset?.id}:${asset?.sourcePath}:${asset?.lastModified}`
  const sourceDuration = asset?.probe?.durationMs ?? 0
  const activeKey = `${asset?.id}:${asset?.sourcePath}:${asset?.lastModified}:${clip?.id}`
  const errorContext = `${activeKey}|${audio.map((entry) => `${entry.clip.id}:${entry.asset?.sourcePath}:${entry.asset?.lastModified}`).join('|')}`
  const error = failure?.context === errorContext ? failure.message : ''
  const available = !!mediaUrl(asset)
  const missing = asset && (!available || asset.linkState !== 'online')
  const invalidStreams = (timeline && resolved.visual && !available) || audio.some((entry) => !mediaUrl(entry.asset))
  const imageFailed = failedImageKey === activeKey
  const canPlay = isDesktop && !busy && !error && !imageFailed && (timeline ? durationFrames > 0 && !invalidStreams : available && asset?.kind !== 'image')
  useEffect(() => { if (asset?.id && asset.kind === 'image') ensureThumbnail(asset.id) }, [asset?.id, asset?.kind, asset?.sourcePath, ensureThumbnail])

  // Explicit UI/MCP seeks are distinct from writes made by the presentation clock.
  useEffect(() => {
    if (!timeline) return
    return useProjectStore.subscribe((state, previous) => {
      if (state.playheadFrame !== previous.playheadFrame && !ownWrite.current) {
        clock.current = { frame: state.playheadFrame, time: performance.now() }
        masterProgress.current = null
        gate.current.active = false
        setSeekEpoch((value) => value + 1)
      }
    })
  }, [timeline])
  useEffect(() => {
    if (!timeline || !playing) {
      evaluate.current = null
      gate.current.active = false
      if (stage.current) { stage.current.dataset.previewPhase = timeline ? 'paused' : 'source'; stage.current.dataset.previewBuffering = 'false' }
      return
    }
    clock.current = { frame: useProjectStore.getState().playheadFrame, time: performance.now() }
    masterProgress.current = null
    let request = 0
    const update = (now: number) => {
      const state = useProjectStore.getState()
      const current = resolveTimelineFrame(project, state.media, state.playheadFrame)
      const owner = current.audio[0] ?? (current.visual?.asset?.kind === 'video' ? current.visual : undefined)
      const lane = owner?.track.kind === 'audio' ? owner.track.id : owner ? 'visual' : undefined
      const node = lane ? nodes.current.get(lane) : undefined
      const expected = [...current.audio.map((entry) => ({ lane: entry.track.id, clip: entry.clip })), ...(current.visual?.asset?.kind === 'video' ? [{ lane: 'visual', clip: current.visual.clip }] : [])]
      const activeNodes = expected.map((entry) => {
        const candidate = nodes.current.get(entry.lane)
        return candidate?.element.dataset.previewClipId === entry.clip.id ? candidate : undefined
      })
      const missingNode = activeNodes.some((entry) => !entry)
      if (!gate.current.active && (missingNode || activeNodes.some((entry) => entry?.needsGate()))) {
        gate.current = { active: true, ticket: gate.current.ticket + 1, target: state.playheadFrame, startedAt: now, masterReleased: false, masterInitialSeconds: 0 }
        masterProgress.current = null
      }
      if (gate.current.active) {
        if (!gate.current.masterReleased) {
          for (const entry of activeNodes) entry?.prepare(gate.current.target, gate.current.ticket)
          clock.current = { frame: gate.current.target, time: now }
        }
        if (!gate.current.masterReleased && (missingNode || activeNodes.some((entry) => !entry?.ready()))) {
          if (stage.current) { stage.current.dataset.previewPhase = 'preparing'; stage.current.dataset.previewBuffering = 'true' }
          if (now - gate.current.startedAt >= 10_000) {
            setPlaying(false)
            setFailure({ context: errorContext, message: '媒体准备超时，请检查素材或重新读取' })
          }
          return
        }
        if (!gate.current.masterReleased) {
          gate.current.masterReleased = true
          gate.current.masterInitialSeconds = node?.element.currentTime ?? 0
          node?.release(gate.current.target)
        }
        if (!node || (node.playConfirmed() && node.ready() && !node.blocked() && !node.element.paused && activeNodes.every((entry) => entry?.ready()) && node.element.currentTime > gate.current.masterInitialSeconds + 0.000001) || node.element.ended) {
          gate.current.active = false
          // Followers enter from their original prepared source positions.
          // Never hide decoder startup delay by skipping unplayed source time.
          for (const entry of activeNodes) if (entry !== node && !node?.element.ended) entry?.release(gate.current.target)
        } else if (now - gate.current.startedAt >= 10_000) {
          setPlaying(false)
          setFailure({ context: errorContext, message: '媒体启动超时，请检查素材或重新读取' })
          return
        }
      }
      let position = clock.current.frame
      let buffering = false
      if (owner) {
        const element = node?.element
        buffering = !element || element.dataset.previewClipId !== owner.clip.id || !!node?.blocked() || element.seeking || element.readyState < 3 || element.paused
        if (element && !buffering && !element.ended) {
          const key = `${element.dataset.previewNodeId}:${owner.clip.id}`
          const previous = masterProgress.current
          if (!previous || previous.key !== key || element.currentTime > previous.seconds + 0.0001) {
            masterProgress.current = { key, seconds: element.currentTime, at: now }
          } else if (now - previous.at >= 250) {
            // Some decoders stop advancing without emitting waiting. Allow
            // coarse 100-ms media clocks, then hold followers instead of
            // repeatedly seeking them back to a silently stalled master.
            buffering = true
          }
        } else masterProgress.current = null
        if (element?.ended && !element.seeking) {
          const sourceEnd = (owner.clip.sourceInFrame + owner.clip.durationFrames) / fps
          if (element.currentTime + Math.max(1 / fps, 0.025) < sourceEnd) {
            setPlaying(false)
            setFailure({ context: errorContext, message: '媒体提前结束，请检查源素材时长' })
            return
          }
          position = owner.clip.startFrame + owner.clip.durationFrames
          buffering = false
        } else if (!buffering && element) {
          position = Math.max(position, Math.min(owner.clip.startFrame + owner.clip.durationFrames, mediaProjectFrame(owner.clip, element.currentTime, fps)))
        }
      } else {
        masterProgress.current = null
        position += Math.max(0, now - clock.current.time) / 1000 * fps
      }
      // Do not skip an intervening media activation on a slow RAF; let its
      // decoder become ready before advancing through that interval.
      let boundary = durationFrames
      for (const track of project.tracks) if (!track.muted) for (const item of track.clips) {
        for (const edge of [item.startFrame, item.startFrame + item.durationFrames]) if (edge > state.playheadFrame) boundary = Math.min(boundary, edge)
      }
      position = Math.min(position, boundary, durationFrames)
      clock.current = { frame: position, time: now }
      if (stage.current) {
        stage.current.dataset.previewBuffering = String(buffering)
        stage.current.dataset.previewPhase = gate.current.active ? 'starting' : buffering ? (node?.element.paused ? 'starting' : 'buffering') : 'playing'
      }
      for (const entry of nodes.current.values()) entry.sync(position, buffering, true)
      const frame = Math.max(0, Math.floor(position + 1e-7))
      ownWrite.current = true
      try { if (state.playheadFrame !== frame) state.setPlayheadFrame(frame) }
      finally { ownWrite.current = false }
      if (frame >= durationFrames) setPlaying(false)
    }
    const tick = (now: number) => { update(now); request = requestAnimationFrame(tick) }
    evaluate.current = () => update(performance.now())
    update(performance.now())
    request = requestAnimationFrame(tick)
    return () => { evaluate.current = null; cancelAnimationFrame(request) }
  }, [durationFrames, fps, playing, timeline, project, errorContext, seekEpoch])
  const play = () => {
    if (!canPlay) throw new Error(error || '当前位置没有可播放的在线媒体，请检查素材状态')
    if (timeline && playheadFrame >= durationFrames) useProjectStore.getState().setPlayheadFrame(0)
    setPlaying(true)
  }
  useEffect(() => {
    const control = (event: Event) => {
      const detail = (event as CustomEvent<TransportRequest>).detail
      try { if (detail.action === 'pause') setPlaying(false); else play(); detail.resolve() }
      catch (error) { detail.reject(error instanceof Error ? error : new Error(String(error))) }
    }
    window.addEventListener('kaicut:transport', control)
    return () => window.removeEventListener('kaicut:transport', control)
  })
  const fail = (message: string) => { setPlaying(false); setFailure({ context: errorContext, message }) }
  const shownError = error || (invalidStreams ? '当前时间线素材离线或已更改，请重新链接后预览' : missing ? '素材离线或已更改，请重新链接' : '')
  // New unavailable source while playing must not silently play a lower layer.
  useEffect(() => {
    if (!playing || !invalidStreams) return
    const task = window.setTimeout(() => { setPlaying(false); setFailure({ context: errorContext, message: '当前时间线素材离线或已更改，请重新链接后预览' }) }, 0)
    return () => window.clearTimeout(task)
  }, [invalidStreams, playing, errorContext])
  const image = available && asset?.kind === 'image' && !imageFailed
    ? asset.thumbnail?.state === 'ready' ? asset.thumbnail.dataUrl : mediaUrl(asset)
    : undefined
  const sourceNode = asset && available && asset.kind !== 'image' && !error
    ? <MediaNode key={stableSourceKey} asset={asset} clip={clip} frame={playheadFrame} fps={fps} playing={playing} gain={1}
      audio={asset.kind === 'audio'} master={timeline && masterLane === 'visual'} seekEpoch={seekEpoch} lane="visual" register={register} wake={wake} onFailure={fail} onTime={timeline ? undefined : setSourceTime} onEnded={timeline ? undefined : () => setPlaying(false)} />
    : image ? <img key={activeKey} className="preview-image" src={image} alt={asset?.name ?? ''} draggable={false} onError={() => { setFailedImageKey(activeKey); setPlaying(false) }} /> : null
  return <div ref={stage} className="preview-stage" data-preview-mode={mode}
    data-preview-clock={timeline ? master?.track.kind ?? 'monotonic' : 'source'} data-preview-clock-clip-id={master?.clip.id ?? ''} data-playing={playing} data-preview-frame={playheadFrame}
    data-preview-visual={clip?.id ?? ''} data-preview-audio={audio.map((entry) => entry.clip.id).join(',')}>
    <div ref={canvas} className="preview-canvas" style={{ background: timeline ? '#000000' : undefined }}>
      {sourceNode}
      {!sourceNode && !shownError && <div className="preview-placeholder" role="status">
        {timeline && durationFrames > 0 ? <span data-preview-gap="true" aria-label="黑场" /> : <><Film size={18} strokeWidth={1.6} /><span>{timeline ? '时间线为空' : '选择素材以开始预览'}</span></>}
      </div>}
      {!timeline && asset?.kind === 'audio' && <div className="preview-placeholder"><FileAudio size={18} strokeWidth={1.6} /><span>{asset.name}</span></div>}
      {audio.map((entry) => entry.asset && mediaUrl(entry.asset) && <MediaNode key={`${entry.track.id}:${entry.asset.id}:${entry.asset.sourcePath}:${entry.asset.lastModified}`} asset={entry.asset} clip={entry.clip}
        frame={playheadFrame} fps={fps} playing={playing} gain={entry.gain} audio master={masterLane === entry.track.id} seekEpoch={seekEpoch} lane={entry.track.id} register={register} wake={wake} onFailure={fail} />)}
      {(shownError || imageFailed) && <div className="preview-playback-error" role="alert">{shownError || '图片无法显示'}
        <Button variant="secondary" size="sm" disabled={busy} onClick={() => { setFailure(null); setFailedImageKey(null); if (asset?.kind === 'image') retryThumbnail(asset.id) }}>重新读取</Button>
      </div>}
    </div>
    <div className="preview-controls">
      <div className="viewer-time" aria-label={timeline ? '时间线时间' : '素材时间'}>
        <span className="timecode">{formatMediaDuration(timeline ? playheadFrame / fps * 1000 : sourceTime * 1000)}</span><span className="timecode-separator">/</span>
        <span className="timecode timecode-total">{formatMediaDuration(timeline ? durationFrames / fps * 1000 : sourceDuration)}</span>
      </div>
      <Button className="preview-play" variant="ghost" size="icon" disabled={!playing && !canPlay} aria-label={playing ? '暂停' : '播放'} onClick={() => { if (playing) setPlaying(false); else { try { play() } catch (error) { fail(String(error)) } } }}>
        {playing ? <Pause size={18} strokeWidth={1.6} /> : <Play size={18} strokeWidth={1.6} />}
      </Button>
      <span className="preview-meta">{timeline ? '时间线' : '素材'} · 适合窗口</span>
    </div>
  </div>
}

export function PreviewArea() {
  const { project, revision, media, previewMode, selectedMediaId, setPreviewMode } = useProjectStore()
  const nativeSourceEnabled = useNativeSourceCapability()
  const key = `${project.projectId}:${revision}:${previewMode}:${previewMode === 'source' ? selectedMediaId : ''}`
  const asset = media.find((item) => item.id === selectedMediaId)
  const nativeSource = nativeSourceEnabled && previewMode === 'source' && asset?.kind === 'video' && asset.linkState === 'online'
  const fallback = <PreviewTransport key={key} project={project} mode={previewMode} sourceId={selectedMediaId} disableNativeHost={nativeSourceEnabled} />
  return <section className="preview-area" aria-label="视频预览">
    <div className="preview-toolbar"><h2>{previewMode === 'timeline' ? '时间线预览' : '素材预览'}</h2>
      <div role="group" aria-label="预览模式" className="preview-meta">
        <Button variant="ghost" size="xs" aria-pressed={previewMode === 'source'} onClick={() => setPreviewMode('source')}>素材</Button>
        <Button variant="ghost" size="xs" aria-pressed={previewMode === 'timeline'} onClick={() => setPreviewMode('timeline')}>时间线</Button>
      </div>
    </div>
    {nativeSource ? <NativeSourcePreview key={key} projectId={project.projectId} revision={revision} mediaId={asset.id} fallback={fallback} /> : fallback}
  </section>
}
