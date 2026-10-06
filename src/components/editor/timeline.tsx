import { ChevronDown, Copy, Eye, Film, Gauge, Lock, Magnet, Music2, Redo2, Scissors, SlidersHorizontal, Trash2, Undo2, Video, Volume2 } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { useProjectStore } from '@/stores/project-store'

function TimelineTool({ label, children }: { label: string; children: React.ReactNode }) {
  return <span className="timeline-tool-tip" data-tooltip={label} title={label}><Button variant="ghost" size="icon" disabled aria-label={`${label}尚未开放`}>{children}</Button></span>
}

export function Timeline() {
  const { tracks, timebase, name } = useProjectStore((state) => state.project)
  // The placeholder still spans 30 seconds; only its ruler density changes.
  const rulerSeconds = Array.from({ length: 30 }, (_, second) => second)
  const thumbnailCells = Array.from({ length: 18 }, (_, index) => index)
  const waveformBars = [4, 8, 12, 7, 15, 10, 18, 8, 13, 6, 16, 10, 19, 7, 12, 5, 15, 9, 17, 8, 13, 6, 16, 10, 18, 7, 12, 5, 15, 9]
  return <section className="timeline-panel" aria-label="时间线">
    <div className="timeline-heading">
      <div className="sequence-title"><Film size={15} strokeWidth={1.5} /><h2 title={name}>{name}</h2><ChevronDown size={10} aria-hidden="true" /><span className="timeline-format">{Number((timebase.fpsNumerator / timebase.fpsDenominator).toFixed(3))} fps</span></div>
      <div className="timeline-tools"><span className="timeline-scope">时间线</span><Button variant="ghost" size="icon" disabled aria-label="吸附尚未开放"><Magnet size={15} strokeWidth={1.5} /></Button><Button variant="ghost" size="icon" disabled aria-label="时间线显示设置尚未开放"><SlidersHorizontal size={15} strokeWidth={1.5} /></Button></div>
    </div>
    <div className="timeline-edit-tools" aria-label="时间线工具">
      <div className="timeline-edit-group">
        <TimelineTool label="撤销"><Undo2 size={16} strokeWidth={1.6} /></TimelineTool>
        <TimelineTool label="重做"><Redo2 size={16} strokeWidth={1.6} /></TimelineTool>
      </div>
      <span className="timeline-tool-divider" aria-hidden="true" />
      <div className="timeline-edit-group">
        <TimelineTool label="分割"><Scissors size={16} strokeWidth={1.6} /></TimelineTool>
        <TimelineTool label="删除"><Trash2 size={16} strokeWidth={1.6} /></TimelineTool>
        <TimelineTool label="复制"><Copy size={16} strokeWidth={1.6} /></TimelineTool>
      </div>
      <span className="timeline-tool-divider" aria-hidden="true" />
      <div className="timeline-edit-group">
        <TimelineTool label="音量"><Volume2 size={16} strokeWidth={1.6} /></TimelineTool>
        <TimelineTool label="速度"><Gauge size={16} strokeWidth={1.6} /></TimelineTool>
      </div>
    </div>
    <div className="timeline-scroll">
      <div className="timeline-ruler-row"><span className="track-label">轨道</span><div className="timeline-ruler" aria-hidden="true">{rulerSeconds.map((second) => <span className="ruler-tick" data-major={second % 5 === 0} key={second}>{second % 5 === 0 && <span className="ruler-label">00:{String(second).padStart(2, '0')}</span>}</span>)}<span className="ruler-playhead" /></div></div>
      <div className="timeline-tracks">
        {tracks.map((track, index) => <div className="timeline-track" key={track.id} role="group" aria-label={`${track.name}，空轨道`}>
          <div className="track-control-row"><div className={`track-icon track-icon-${track.kind}`} title={track.kind === 'video' ? '视频轨道' : '音频轨道'}>{track.kind === 'video' ? <Video size={15} strokeWidth={1.6} /> : <Music2 size={15} strokeWidth={1.6} />}</div><div className="track-control-actions"><TimelineTool label="锁定"><Lock size={12} strokeWidth={1.6} /></TimelineTool><TimelineTool label="隐藏"><Eye size={13} strokeWidth={1.6} /></TimelineTool></div></div>
          <div className={`track-row track-row-${track.kind}`}><span className="track-origin" />{track.kind === 'video' && index === 0 && <span className="timeline-visual-clip timeline-video-clip" aria-hidden="true">{thumbnailCells.map((cell) => <i key={cell} />)}</span>}{track.kind === 'audio' && index === 2 && <span className="timeline-visual-clip timeline-audio-clip" aria-hidden="true">{waveformBars.map((height, bar) => <i key={bar} style={{ height: `${height}px` }} />)}</span>}{index === 1 && <span className="timeline-empty-note">暂无片段</span>}</div>
        </div>)}
      </div>
    </div>
    <div className="timeline-footer"><span>{tracks.length} 条轨道</span><span>片段编辑尚未开放</span><span className="sequence-duration">00:00:00:00</span></div>
  </section>
}
