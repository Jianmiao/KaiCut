import { useId, useState, type FormEvent } from 'react'
import { useShallow } from 'zustand/react/shallow'
import { Info, Film, Plus } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { isDesktop, useProjectStore } from '@/stores/project-store'
import { formatMediaDuration } from '@/lib/media-display'
import { maxTimelineFrames } from '@/lib/kaicut-format'
import type { MediaAsset, TimelineClip, TimelineTrack } from '@/types/project'
import './inspector-editing.css'

function formatFrameRate(numerator?: number, denominator?: number) {
  if (!numerator || !denominator) return '—'
  const rate = numerator / denominator
  return denominator === 1 ? `${numerator} fps` : `${rate.toFixed(rate >= 10 ? 2 : 3)} fps (${numerator}/${denominator})`
}

function formatSampleRate(sampleRate?: number) {
  if (!sampleRate) return '—'
  return `${(sampleRate / 1000).toFixed(sampleRate % 1000 === 0 ? 0 : 1)} kHz`
}

function formatChannels(channels?: number) {
  if (!channels) return '—'
  if (channels === 1) return '单声道'
  if (channels === 2) return '双声道'
  return `${channels} 声道`
}

export function Inspector() {
  const { project, projectPath, media, selectedMediaId, selectedClipId, relinkMedia } = useProjectStore(useShallow((state) => ({
    project: state.project, projectPath: state.projectPath, media: state.media,
    selectedMediaId: state.selectedMediaId, selectedClipId: state.selectedClipId, relinkMedia: state.relinkMedia,
  })))
  const track = project.tracks.find((item) => item.clips.some((clip) => clip.id === selectedClipId))
  const clip = track?.clips.find((item) => item.id === selectedClipId)
  const asset = media.find((item) => item.id === (clip?.mediaId ?? selectedMediaId))
  return <aside className="panel-shell inspector" aria-label="检查器">
    <div className="panel-heading"><h2>{clip ? '片段属性' : asset ? '素材属性' : '项目参数'}</h2><Info size={16} strokeWidth={1.6} className="muted-icon" /></div>
    {asset ? <div className="inspector-section">
      <div className="inspector-file"><span className={`file-type-symbol file-type-${asset.kind}`}><Info size={18} strokeWidth={1.5} /></span><div><h3 className="selected-file-name">{asset.name}</h3><span className="file-subtitle">{{ video: '视频素材', audio: '音频素材', image: '图片素材' }[asset.kind]}</span></div></div>
      {(asset.linkState === 'missing' || asset.linkState === 'changed' || asset.thumbnail?.state === 'missing' || asset.thumbnail?.state === 'changed') && <div className={`inspector-link-status inspector-link-${asset.linkState}`}><span>{asset.thumbnail?.error ?? asset.linkReason ?? '请重新链接原始素材'}</span><Button variant="secondary" size="sm" onClick={() => void relinkMedia(asset.id)}>重新链接</Button></div>}
      {clip && track ? <ClipEditor key={`${project.projectId}:${clip.id}:${clip.trackId}:${clip.startFrame}:${clip.sourceInFrame}:${clip.durationFrames}:${clip.sourceDurationFrames}`} clip={clip} track={track} /> : <AddToTimeline key={`${project.projectId}:${asset.id}`} asset={asset} />}
      <div className="inspector-group-title">文件信息</div>
      <dl className="metadata"><dt>格式</dt><dd>{asset.extension.toUpperCase()}</dd><dt>大小</dt><dd>{asset.size < 1024 * 1024 ? `${Math.max(1, Math.round(asset.size / 1024))} KB` : `${(asset.size / 1024 / 1024).toFixed(2)} MB`}</dd><dt>位置</dt><dd className="source-path" title={asset.sourcePath}>{asset.sourcePath ?? '浏览器选取'}</dd></dl>
      {asset.probe && <>
        <div className="inspector-group-title">媒体信息</div>
        <dl className="metadata">
          <dt>探测状态</dt><dd>{asset.probe.state === 'probing' ? '探测中…' : asset.probe.state === 'ready' ? '已读取' : asset.probe.state === 'missing' ? '文件缺失' : asset.probe.state === 'unlinked' ? '未链接' : asset.probe.state === 'failed' ? '读取失败' : '等待探测'}</dd>
          {asset.probe.state === 'ready' && <>
            {asset.kind !== 'image' && <><dt>时长</dt><dd>{formatMediaDuration(asset.probe.durationMs)}</dd></>}
            {(asset.kind === 'video' || asset.kind === 'image') && <><dt>分辨率</dt><dd>{asset.probe.width && asset.probe.height ? `${asset.probe.width} × ${asset.probe.height}` : '—'}</dd></>}
            {asset.kind === 'video' && <><dt>帧率</dt><dd>{formatFrameRate(asset.probe.fpsNumerator, asset.probe.fpsDenominator)}</dd></>}
            {(asset.kind === 'audio' || asset.probe.audioSampleRate !== undefined || asset.probe.audioChannels !== undefined) && <><dt>采样率</dt><dd>{formatSampleRate(asset.probe.audioSampleRate)}</dd><dt>声道</dt><dd>{formatChannels(asset.probe.audioChannels)}</dd></>}
            {asset.probe.rotation !== undefined && <><dt>旋转</dt><dd>{asset.probe.rotation}°</dd></>}
          </>}
        </dl>
        {asset.probe.error && <p className="inspector-probe-error">{asset.probe.error}</p>}
      </>}
    </div> : <div className="inspector-section project-properties">
      <div className="inspector-file"><span className="file-type-symbol"><Film size={18} strokeWidth={1.6} /></span><div><h3 className="selected-file-name">{project.name}</h3><span className="file-subtitle">本地项目</span></div></div>
      <dl className="metadata">
        <dt>项目名称</dt><dd>{project.name}</dd><dt>保存位置</dt><dd className="source-path" title={projectPath ?? undefined}>{projectPath ?? '尚未保存'}</dd>
        <dt>项目帧率</dt><dd>{formatFrameRate(project.timebase.fpsNumerator, project.timebase.fpsDenominator)}</dd><dt>素材数量</dt><dd>{media.length} 项</dd>
        <dt>媒体存储</dt><dd>保留在原位置</dd>
      </dl>
      <p className="project-properties-hint">选择素材后在这里查看详细信息</p>
    </div>}
  </aside>
}

function AddToTimeline({ asset }: { asset: MediaAsset }) {
  const { project, busy, editTimeline } = useProjectStore(useShallow((state) => ({ project: state.project, busy: state.busy, editTimeline: state.editTimeline })))
  const kind = asset.kind === 'audio' ? 'audio' : 'video'
  const tracks = project.tracks.filter((track) => track.kind === kind)
  const [requestedTrackId, setRequestedTrackId] = useState('')
  const [error, setError] = useState('')
  const track = tracks.find((item) => item.id === requestedTrackId) ?? tracks.find((item) => !item.locked)
  const reason = !isDesktop ? '请在桌面版添加片段' : busy ? '请等待当前操作完成' : asset.linkState !== 'online' ? '请先确认素材在线或重新链接' : asset.probe?.state !== 'ready' ? '请等待素材信息读取成功' : asset.kind !== 'image' && (!asset.probe.durationMs || !Number.isFinite(asset.probe.durationMs)) ? '无法读取素材时长，暂不能添加' : !track ? '没有可用的同类型轨道' : track.locked ? '目标轨道已锁定' : ''
  const add = async () => {
    if (reason || !track) return
    setError('')
    await editTimeline({ type: 'add', mediaId: asset.id, trackId: track.id })
    const current = useProjectStore.getState()
    if (current.project.projectId === project.projectId && current.statusMessage.startsWith('操作未完成：')) setError(current.statusMessage)
  }
  return <div className="inspector-clip-editing">
    <div className="inspector-group-title">添加片段</div>
    <Select aria-label="目标轨道" options={tracks.map((item) => ({ value: item.id, label: `${item.name}${item.locked ? '（已锁定）' : ''}`, disabled: item.locked }))} value={track?.id ?? ''} onChange={(id) => { setRequestedTrackId(id); setError('') }} disabled={busy || !isDesktop || tracks.every((item) => item.locked)} placeholder="选择目标轨道" />
    <Button className="inspector-add-clip" variant="secondary" size="sm" disabled={!!reason} title={reason || '追加到目标轨道末尾'} onClick={() => void add()}><Plus size={16} strokeWidth={1.6} aria-hidden="true" />添加到时间线</Button>
    <p className="inspector-edit-hint">{reason || (asset.kind === 'image' ? '图片默认显示 5 秒，追加到轨道末尾' : '追加到轨道末尾，保留原始素材')}</p>
    {error && <p role="alert" className="inspector-probe-error">{error}</p>}
  </div>
}

function frameValue(value: string) {
  if (!/^\d+$/.test(value)) return undefined
  const number = Number(value)
  return Number.isSafeInteger(number) ? number : undefined
}

function ClipEditor({ clip, track }: { clip: TimelineClip; track: TimelineTrack }) {
  const { project, busy, editTimeline } = useProjectStore(useShallow((state) => ({ project: state.project, busy: state.busy, editTimeline: state.editTimeline })))
  const inputId = useId()
  const [trackId, setTrackId] = useState(track.id)
  const [start, setStart] = useState(String(clip.startFrame))
  const [sourceIn, setSourceIn] = useState(String(clip.sourceInFrame))
  const [duration, setDuration] = useState(String(clip.durationFrames))
  const [error, setError] = useState('')
  const disabled = !isDesktop || busy || track.locked
  const tracks = project.tracks.filter((item) => item.kind === track.kind)
  const limit = maxTimelineFrames(project.timebase)
  const reportResult = () => {
    const current = useProjectStore.getState()
    if (current.project.projectId === project.projectId && current.selectedClipId === clip.id && current.statusMessage.startsWith('操作未完成：')) setError(current.statusMessage)
  }
  const move = async (event: FormEvent) => {
    event.preventDefault()
    if (disabled) return
    const startFrame = frameValue(start)
    if (startFrame === undefined || startFrame + clip.durationFrames > limit) { setError('片段起点需为有效的非负整数帧，且不能超出时间线范围'); return }
    if (!tracks.some((item) => item.id === trackId && !item.locked)) { setError('请选择未锁定的同类型轨道'); return }
    setError('')
    await editTimeline({ type: 'move', clipId: clip.id, trackId, startFrame })
    reportResult()
  }
  const trim = async (event: FormEvent) => {
    event.preventDefault()
    if (disabled) return
    const sourceInFrame = frameValue(sourceIn)
    const durationFrames = frameValue(duration)
    if (sourceInFrame === undefined || durationFrames === undefined || durationFrames < 1) { setError('源入点需为非负整数帧，时长至少为 1 帧'); return }
    if (sourceInFrame + durationFrames > clip.sourceDurationFrames || clip.startFrame + durationFrames > limit) { setError('裁切范围不能超出源素材或时间线范围'); return }
    setError('')
    await editTimeline({ type: 'trim', clipId: clip.id, sourceInFrame, startFrame: clip.startFrame, durationFrames })
    reportResult()
  }
  return <div className="inspector-clip-editing">
    <div className="inspector-group-title">时间线位置</div>
    <p className="inspector-edit-hint">按项目帧率计数 · 起点从第 0 帧开始{track.locked ? ' · 当前轨道已锁定' : ''}</p>
    <form className="inspector-edit-form" onSubmit={(event) => void move(event)}>
      <label htmlFor={`${inputId}-track`}>所在轨道</label><Select id={`${inputId}-track`} options={tracks.map((item) => ({ value: item.id, label: `${item.name}${item.locked ? '（已锁定）' : ''}`, disabled: item.locked }))} value={trackId} onChange={setTrackId} disabled={disabled} />
      <label htmlFor={`${inputId}-start`}>片段起点（帧）</label><Input id={`${inputId}-start`} type="number" min="0" max={limit - clip.durationFrames} step="1" value={start} onChange={(event) => setStart(event.target.value)} disabled={disabled} required />
      <Button className="inspector-edit-submit" type="submit" variant="secondary" size="sm" disabled={disabled}>移动片段</Button>
    </form>
    <div className="inspector-group-title">源素材范围</div>
    <form className="inspector-edit-form" onSubmit={(event) => void trim(event)}>
      <label htmlFor={`${inputId}-source`}>源入点（帧）</label><Input id={`${inputId}-source`} type="number" min="0" max={clip.sourceDurationFrames - 1} step="1" value={sourceIn} onChange={(event) => setSourceIn(event.target.value)} disabled={disabled} required />
      <label htmlFor={`${inputId}-duration`}>时长（帧）</label><Input id={`${inputId}-duration`} type="number" min="1" max={clip.sourceDurationFrames} step="1" value={duration} onChange={(event) => setDuration(event.target.value)} disabled={disabled} required />
      <Button className="inspector-edit-submit" type="submit" variant="secondary" size="sm" disabled={disabled}>应用裁切</Button>
    </form>
    <p className="inspector-edit-hint">保留时间线起点，只修改源素材范围。原始文件不变。</p>
    {error && <p role="alert" className="inspector-probe-error">{error}</p>}
  </div>
}
