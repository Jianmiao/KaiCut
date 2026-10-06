import { Info, MousePointer2 } from 'lucide-react'
import { useProjectStore } from '@/stores/project-store'

export function Inspector() {
  const { media, selectedMediaId } = useProjectStore()
  const asset = media.find((item) => item.id === selectedMediaId)
  return <aside className="panel-shell inspector" aria-label="检查器">
    <div className="panel-heading"><h2>检查器</h2><Info size={15} strokeWidth={1.5} className="muted-icon" /></div>
    {asset ? <div className="inspector-section">
      <div className="inspector-file"><span className={`file-type-symbol file-type-${asset.kind}`}><Info size={18} strokeWidth={1.5} /></span><div><h3 className="selected-file-name">{asset.name}</h3><span className="file-subtitle">{{ video: '视频素材', audio: '音频素材', image: '图片素材' }[asset.kind]}</span></div></div>
      <div className="inspector-group-title">文件信息</div>
      <dl className="metadata"><dt>格式</dt><dd>{asset.extension.toUpperCase()}</dd><dt>大小</dt><dd>{asset.size < 1024 * 1024 ? `${Math.max(1, Math.round(asset.size / 1024))} KB` : `${(asset.size / 1024 / 1024).toFixed(2)} MB`}</dd><dt>位置</dt><dd className="source-path" title={asset.sourcePath}>{asset.sourcePath ?? '浏览器选取'}</dd></dl>
    </div> : <div className="inspector-empty"><MousePointer2 size={20} strokeWidth={1} className="muted-icon" /><strong>未选择素材</strong><p>选择素材以查看属性</p></div>}
  </aside>
}
