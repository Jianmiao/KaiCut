import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { MediaBin } from './media-bin'
import { useProjectStore } from '@/stores/project-store'
import { createEmptyProject } from '@/lib/kaicut-format'
import type { MediaAsset } from '@/types/project'

vi.mock('@tauri-apps/api/core', () => ({ isTauri: () => true, invoke: vi.fn() }))

class FakeIntersectionObserver implements IntersectionObserver {
  static instances: FakeIntersectionObserver[] = []
  readonly root: Element | Document | null
  readonly rootMargin: string
  readonly thresholds = [0]
  readonly scrollMargin = '0px'
  readonly delay = 0
  readonly trackVisibility = false
  private targets = new Set<Element>()
  private callback: IntersectionObserverCallback
  disconnect = vi.fn(() => this.targets.clear())

  constructor(callback: IntersectionObserverCallback, options?: IntersectionObserverInit) {
    this.callback = callback
    this.root = options?.root ?? null
    this.rootMargin = options?.rootMargin ?? '0px'
    FakeIntersectionObserver.instances.push(this)
  }

  observe(target: Element) { this.targets.add(target) }
  unobserve(target: Element) { this.targets.delete(target) }
  takeRecords(): IntersectionObserverEntry[] { return [] }
  emit(isIntersecting: boolean) {
    const entries = Array.from(this.targets, (target): IntersectionObserverEntry => ({
      target,
      isIntersecting,
      intersectionRatio: isIntersecting ? 1 : 0,
      time: 0,
      boundingClientRect: target.getBoundingClientRect(),
      intersectionRect: target.getBoundingClientRect(),
      rootBounds: null,
    }))
    this.callback(entries, this)
  }
}

let host: HTMLDivElement
let root: Root
const ensure = vi.fn()
const originalEnsure = useProjectStore.getState().ensureThumbnail
const asset: MediaAsset = { id: 'v', name: '中文 片段.mp4', kind: 'video', extension: 'mp4', size: 100, lastModified: 200, sourcePath: 'E:/素材.mp4', linkState: 'online' }

beforeEach(async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true })
  FakeIntersectionObserver.instances = []
  vi.stubGlobal('IntersectionObserver', FakeIntersectionObserver)
  ensure.mockClear()
  useProjectStore.setState({ project: createEmptyProject(), media: [{ ...asset }], selectedMediaId: null, busy: false, ensureThumbnail: ensure })
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
  await act(async () => root.render(<MediaBin />))
})

afterEach(async () => {
  await act(async () => root.unmount())
  host.remove()
  useProjectStore.setState({ ensureThumbnail: originalEnsure })
  vi.unstubAllGlobals()
})

describe('media card thumbnail visibility', () => {
  it('requests a thumbnail only on entering the viewport and stops observing on unmount', async () => {
    const observer = FakeIntersectionObserver.instances[0]!
    expect(FakeIntersectionObserver.instances).toHaveLength(1)
    expect(observer.root).toBe(host.querySelector('.media-list-grid'))
    expect(ensure).not.toHaveBeenCalled()
    await act(async () => observer.emit(false))
    expect(ensure).not.toHaveBeenCalled()
    await act(async () => observer.emit(true))
    expect(ensure).toHaveBeenCalledExactlyOnceWith(asset.id)
    await act(async () => observer.emit(false))
    expect(ensure).toHaveBeenCalledTimes(1)
    await act(async () => root.render(null))
    expect(observer.disconnect).toHaveBeenCalledTimes(1)
  })

  it('restarts observation when a different project reuses an identical media reference', async () => {
    const previous = FakeIntersectionObserver.instances[0]!
    await act(async () => previous.emit(true))
    await act(async () => useProjectStore.setState({ project: createEmptyProject(), media: [{ ...asset }] }))
    expect(previous.disconnect).toHaveBeenCalledTimes(1)
    expect(FakeIntersectionObserver.instances).toHaveLength(2)
    expect(ensure).toHaveBeenCalledTimes(1)
    await act(async () => previous.emit(true))
    expect(ensure).toHaveBeenCalledTimes(1)
    await act(async () => FakeIntersectionObserver.instances[1]!.emit(true))
    expect(ensure).toHaveBeenCalledTimes(2)
    expect(ensure).toHaveBeenLastCalledWith(asset.id)
  })

  it('does not request again on thumbnail updates or eviction until the card re-enters view', async () => {
    const observer = FakeIntersectionObserver.instances[0]!
    await act(async () => observer.emit(true))
    for (const thumbnail of [{ state: 'loading' as const }, { state: 'ready' as const, dataUrl: 'data:image/png;base64,frame' }, undefined]) {
      await act(async () => useProjectStore.setState({ media: [{ ...asset, thumbnail }] }))
    }
    expect(FakeIntersectionObserver.instances).toHaveLength(1)
    expect(observer.disconnect).not.toHaveBeenCalled()
    expect(ensure).toHaveBeenCalledTimes(1)
    await act(async () => observer.emit(false))
    expect(ensure).toHaveBeenCalledTimes(1)
    await act(async () => observer.emit(true))
    expect(ensure).toHaveBeenCalledTimes(2)
  })

  it('shows a completed thumbnail once the asynchronous link check comes back online', async () => {
    const thumbnail = { state: 'ready' as const, dataUrl: 'data:image/png;base64,frame' }
    await act(async () => useProjectStore.setState({ media: [{ ...asset, thumbnail, linkState: 'unknown' }] }))
    expect(host.querySelector('.media-thumbnail-image')).toBeNull()
    await act(async () => useProjectStore.setState({ media: [{ ...asset, thumbnail, linkState: 'online' }] }))
    expect(host.querySelector('.media-thumbnail-image')?.getAttribute('src')).toBe(thumbnail.dataUrl)
    expect(FakeIntersectionObserver.instances).toHaveLength(1)
    expect(ensure).not.toHaveBeenCalled()
  })
})
