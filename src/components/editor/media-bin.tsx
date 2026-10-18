import { useEffect, useRef, useState } from 'react'
import { FileAudio, FileImage, FileVideo, FolderOpen, Grid2X2, List, Plus, Search, Upload } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Tabs, TabsList, TabsTrigger, TabsContent } from '@/components/ui/tabs'
import { Sidebar, SidebarProvider } from '@/components/ui/sidebar'
import { isDesktop, useProjectStore } from '@/stores/project-store'
import type { MediaAsset } from '@/types/project'

function MediaIcon({ kind }: { kind: MediaAsset['kind'] }) {
  if (kind === 'image') return <FileImage className="media-type-icon media-type-image" />
  if (kind === 'audio') return <FileAudio className="media-type-icon media-type-audio" />
  return <FileVideo className="media-type-icon media-type-video" />
}

function formatBytes(bytes: number) {
  if (bytes < 1024 * 1024) return `${Math.max(1, Math.round(bytes / 1024))} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

export function MediaBin() {
  const inputRef = useRef<HTMLInputElement>(null)
  const [view, setView] = useState('grid')
  const [query, setQuery] = useState('')
  const media = useProjectStore((state) => state.media)
  const importMedia = useProjectStore((state) => state.importMedia)
  const busy = useProjectStore((state) => state.busy)
  const importFiles = () => { if (isDesktop) void importMedia(); else inputRef.current?.click() }
  const visibleMedia = media.filter((asset) => asset.name.toLowerCase().includes(query.toLowerCase()))

  return (
    <SidebarProvider className="media-bin-provider" width="w-full"><Sidebar collapsible={false} className="panel-shell media-bin" aria-label="素材库">
      <div className="panel-heading">
        <div>
          <h2 className="library-heading"><FolderOpen size={16} strokeWidth={1.6} aria-hidden="true" />素材<span className="count-badge">{media.length}</span></h2>
        </div>
        <Button variant="secondary" size="sm" aria-label="添加媒体文件" title="导入媒体" className="media-import-action" disabled={busy} onClick={importFiles}><Plus className="icon-sm" />导入</Button>
      </div>
      <Tabs value={view} onValueChange={setView} className="media-tabs">
      <div className="media-toolbar">
        <div className="search-wrap">
          <Search className="search-icon" aria-hidden="true" />
          <Input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="搜索素材" aria-label="搜索素材" />
        </div>
          <TabsList aria-label="素材视图" className="media-view-switch">
            <TabsTrigger value="list" aria-label="列表视图"><List className="icon-xs" /></TabsTrigger>
            <TabsTrigger value="grid" aria-label="缩略图视图"><Grid2X2 className="icon-xs" /></TabsTrigger>
          </TabsList>
      </div>
      <input
        ref={inputRef}
        hidden
        type="file"
        multiple
        accept=".mp4,.mov,.mp3,.wav,.png,.jpg,.jpeg,video/mp4,video/quicktime,audio/mpeg,audio/wav,image/png,image/jpeg"
        onChange={(event) => {
          if (event.target.files?.length) void importMedia(event.target.files)
          event.currentTarget.value = ''
        }}
      />

      {['list', 'grid'].map((mode) => <TabsContent value={mode} key={mode} className={`media-list ${mode === 'grid' ? 'media-list-grid' : ''}`}>
        {visibleMedia.length === 0 ? (
          <div className="empty-state media-empty">
            {query ? <><Search className="library-empty-symbol" strokeWidth={1.6} aria-hidden="true" /><strong>没有匹配素材</strong><p>尝试其他文件名</p></> : <>
              <Button variant="secondary" className="media-import-tile" disabled={busy} onClick={importFiles}><span className="media-import-symbol"><Upload size={20} strokeWidth={1.6} /></span><span>导入媒体</span><small>视频、音频、图片</small></Button>
              <p className="media-local-hint">文件保留在原位置</p>
            </>}
          </div>
        ) : visibleMedia.map((asset) => (
          <MediaCard key={asset.id} asset={asset} grid={mode === 'grid'} />
        ))}
      </TabsContent>)}
      </Tabs>
      <div className="panel-footer"><span>{visibleMedia.length} 个项目</span><span>本地素材</span></div>
    </Sidebar></SidebarProvider>
  )
}

function MediaCard({ asset, grid }: { asset: MediaAsset; grid: boolean }) {
  const select = useProjectStore((state) => state.selectMedia)
  const selected = useProjectStore((state) => state.selectedMediaId === asset.id)
  const ensureThumbnail = useProjectStore((state) => state.ensureThumbnail)
  const projectId = useProjectStore((state) => state.project.projectId)
  const cardRef = useRef<HTMLButtonElement>(null)
  useEffect(() => {
    const card = cardRef.current
    if (!card || !isDesktop || asset.kind === 'audio') return
    if (typeof IntersectionObserver === 'undefined') { ensureThumbnail(asset.id); return }
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) ensureThumbnail(asset.id)
    }, { root: card.closest('.media-list'), rootMargin: '80px' })
    observer.observe(card)
    return () => observer.disconnect()
  }, [projectId, asset.id, asset.kind, asset.sourcePath, asset.size, asset.lastModified, ensureThumbnail])
  const thumbnail = asset.thumbnail
  const image = thumbnail?.state === 'ready' && asset.linkState === 'online' ? thumbnail.dataUrl : undefined
  const add = () => {
    const state = useProjectStore.getState()
    if (!isDesktop || state.busy || asset.linkState !== 'online' || asset.probe?.state !== 'ready') return
    const track = state.project.tracks.find((item) => item.kind === (asset.kind === 'audio' ? 'audio' : 'video') && !item.locked)
    if (track) void state.editTimeline({ type: 'add', mediaId: asset.id, trackId: track.id })
    else state.setStatusMessage('没有可用的同类型轨道，请先解锁轨道')
  }
  return (
    <button ref={cardRef} type="button" className={`media-card ${grid ? 'media-card-grid' : ''}`} aria-pressed={selected} onClick={() => select(asset.id)} onDoubleClick={add} title="双击追加到可用轨道；也可在检查器选择目标轨道" aria-label={`${asset.name}，${formatBytes(asset.size)}${asset.linkState === 'missing' ? '，素材已离线' : asset.linkState === 'changed' ? '，素材已更改' : ''}`}>
      <div className="media-thumb">
        {image ? <img className="media-thumbnail-image" src={image} alt="" draggable={false} /> : <MediaIcon kind={asset.kind} />}
        <span className="media-extension">{asset.extension.toUpperCase()}</span>
        {thumbnail?.state === 'loading' && <span className="media-link-badge">读取中</span>}
        {thumbnail?.state === 'failed' && <span className="media-link-badge" title={thumbnail.error}>预览失败</span>}
        {asset.linkState === 'missing' && <span className="media-link-badge">离线</span>}
        {asset.linkState === 'changed' && <span className="media-link-badge">已更改</span>}
      </div>
      <div className="media-copy">
        <strong title={asset.name}>{asset.name}</strong>
        <span>{asset.extension.toUpperCase()} · {formatBytes(asset.size)}</span>
      </div>
    </button>
  )
}
