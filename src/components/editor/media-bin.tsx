import { useRef, useState } from 'react'
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
          <h2>素材库 <span className="count-badge">{media.length}</span></h2>
        </div>
        <Button variant="ghost" size="icon" aria-label="添加媒体文件" title="导入媒体" className="panel-action" disabled={busy} onClick={importFiles}><Plus className="icon-sm" /></Button>
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
            <FolderOpen className="library-empty-symbol" strokeWidth={1} aria-hidden="true" />
            <strong>{query ? '没有匹配素材' : '项目中还没有素材'}</strong>
            <p>{query ? '尝试其他文件名' : '添加视频、音频或图片'}</p>
            {!query && <Button variant="secondary" className="import-button" disabled={busy} onClick={importFiles}><Upload size={14} strokeWidth={1.6} />导入媒体</Button>}
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
  return (
    <button type="button" className={`media-card ${grid ? 'media-card-grid' : ''}`} aria-pressed={selected} onClick={() => select(asset.id)} aria-label={`${asset.name}，${formatBytes(asset.size)}`}>
      <div className="media-thumb">
        {asset.objectUrl ? <img src={asset.objectUrl} alt="" /> : <MediaIcon kind={asset.kind} />}
        <span className="media-extension">{asset.extension.toUpperCase()}</span>
      </div>
      <div className="media-copy">
        <strong title={asset.name}>{asset.name}</strong>
        <span>{asset.extension.toUpperCase()} · {formatBytes(asset.size)}</span>
      </div>
    </button>
  )
}
