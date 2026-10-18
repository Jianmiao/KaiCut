import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react'
import { Film, Pause, Play } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { useNativePreviewHost, type NativeHostStatus } from '@/hooks/use-native-preview-host'
import { useNativeSourcePreview } from '@/hooks/use-native-source-preview'
import { formatMediaDuration } from '@/lib/media-display'
import type { TransportRequest } from './preview-area'
import '@/styles/native-source.css'

/** Native pixels occupy only the existing canvas; DOM controls stay outside it. */
export function NativeSourcePreview({ projectId, revision, mediaId, fallback }: {
  projectId: string; revision: number; mediaId: string; fallback: ReactNode
}) {
  const canvas = useRef<HTMLDivElement>(null)
  const [hostReady, setHostReady] = useState(false)
  const [hostFailed, setHostFailed] = useState(false)
  const [draft, setDraft] = useState<string | null>(null)
  const [inputError, setInputError] = useState('')
  const source = useNativeSourcePreview({ projectId, revision, mediaId, enabled: hostReady, hostFailed })
  const compatibility = source.mode === 'fallback'
  const onHostStatus = useCallback((status: NativeHostStatus) => {
    if (status === 'attached') setHostReady(true)
    else if (status === 'fallback' || status === 'disabled') setHostFailed(true)
  }, [])
  useNativePreviewHost(canvas, { enabled: !compatibility && !hostFailed && source.mode !== 'error', onStatus: onHostStatus })
  const { control, snapshot } = source
  useEffect(() => {
    if (compatibility) return
    const receive = (event: Event) => {
      const request = (event as CustomEvent<TransportRequest>).detail
      void control(request.action).then(request.resolve, request.reject)
    }
    window.addEventListener('kaicut:transport', receive)
    return () => window.removeEventListener('kaicut:transport', receive)
  }, [compatibility, control])
  if (compatibility) return <>{fallback}<span className="sr-only" role="status">使用兼容预览</span></>
  const ready = source.mode === 'native' && !hostFailed
  const playing = snapshot?.phase === 'playing'
  const position = Number(snapshot?.positionNs ?? '0') / 1e9
  const duration = Number(snapshot?.durationNs ?? '0') / 1e9
  const commitSeek = (value: string) => {
    const seconds = Number(value)
    setDraft(null)
    if (value.trim() === '' || !Number.isFinite(seconds) || seconds < 0) {
      setInputError('请输入不小于 0 的秒数')
      return
    }
    setInputError('')
    void control('seek', String(Math.round(Math.min(seconds, duration) * 1e9))).catch(() => {})
  }
  return <div className="preview-stage native-source-preview" data-preview-mode="source" data-preview-engine="native-source"
    data-preview-clock="native-audio" data-preview-phase={snapshot?.phase ?? 'starting'} data-playing={playing}
    data-preview-media-id={mediaId} data-preview-position-ns={snapshot?.positionNs ?? '0'}>
    <div ref={canvas} className="preview-canvas">
      {!ready && <div className="preview-placeholder" role={source.mode === 'error' ? 'alert' : 'status'}><Film size={18} strokeWidth={1.6} />
        <span>{source.mode === 'error' ? '预览已停止，请重新选择素材' : '正在准备预览'}</span></div>}
    </div>
    <div className="preview-controls">
      <div className="viewer-time" aria-label="素材时间">
        <span className="timecode">{formatMediaDuration(position * 1000)}</span><span className="timecode-separator">/</span>
        <span className="timecode timecode-total">{formatMediaDuration(duration * 1000)}</span>
      </div>
      <Button className="preview-play" variant="ghost" size="icon" disabled={!ready} aria-label={playing ? '暂停' : '播放'}
        onClick={() => { void control(playing ? 'pause' : 'play').catch(() => {}) }}>
        {playing ? <Pause size={16} strokeWidth={1.6} /> : <Play size={16} strokeWidth={1.6} />}
      </Button>
      <label className="native-source-seek">
        <Input type="number" min={0} max={duration} step={0.001} aria-label="素材定位（秒）" aria-invalid={!!inputError}
          className="native-source-seek-input" disabled={!ready} value={draft ?? position.toFixed(3)}
          onFocus={() => setDraft(position.toFixed(3))} onChange={(event) => setDraft(event.target.value)}
          onBlur={(event) => { if (draft !== null) commitSeek(event.target.value) }}
          onKeyDown={(event) => { if (event.key === 'Enter') { event.preventDefault(); event.currentTarget.blur() } }} />
        <span aria-hidden="true">秒</span>
      </label>
      <span className="sr-only" role="status">{inputError || (snapshot?.phase === 'ended' ? '播放结束，可重新播放' : snapshot?.muted ? '静音预览' : '')}</span>
    </div>
  </div>
}
