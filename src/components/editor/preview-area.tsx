import { ChevronDown, Film, Maximize2, Play, ScanSearch, Volume2 } from 'lucide-react'
import { Button } from '@/components/ui/button'

export function PreviewArea() {
  return <section className="preview-area" aria-label="视频预览">
    <div className="preview-toolbar"><h2>查看器</h2><div className="preview-meta"><span>1080p</span><span className="meta-divider">·</span><span>适合窗口</span><ChevronDown size={10} aria-hidden="true" /></div></div>
    <div className="preview-stage"><div className="preview-canvas">
      <div className="preview-placeholder"><Film size={18} strokeWidth={1.6} aria-hidden="true" /><span>预览画面</span><small>播放将在后续版本开放</small></div>
    </div>
    <div className="preview-controls">
      <div className="viewer-time"><span className="timecode">00:00:00<span className="timecode-frames">:00</span></span><span className="timecode-separator">/</span><span className="timecode timecode-total">00:00:00<span className="timecode-frames">:00</span></span></div>
      <Button className="preview-play" variant="ghost" size="icon" disabled aria-label="播放尚未开放"><Play size={18} strokeWidth={1.6} /></Button>
      <div className="viewer-options"><Button variant="ghost" size="sm" disabled aria-label="原画尚未开放">原画</Button><Button variant="ghost" size="icon" disabled aria-label="画面缩放尚未开放"><ScanSearch size={16} strokeWidth={1.6} /></Button><Button variant="ghost" size="sm" disabled aria-label="比例尚未开放">比例</Button><Button variant="ghost" size="icon" disabled aria-label="音频监听尚未开放"><Volume2 size={16} strokeWidth={1.6} /></Button><Button variant="ghost" size="icon" disabled aria-label="全屏预览尚未开放"><Maximize2 size={15} strokeWidth={1.6} /></Button></div>
    </div>
    </div>
  </section>
}
