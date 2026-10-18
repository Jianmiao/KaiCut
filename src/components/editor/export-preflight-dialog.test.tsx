import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ExportPreflightDialog } from './export-preflight-dialog'
import { createEmptyProject } from '@/lib/kaicut-format'
import type { ExportJobRecord, ExportPreflightResult } from '@/types/export'

const fixture = vi.hoisted(() => ({ invoke: vi.fn() }))
vi.mock('@tauri-apps/api/core', () => ({ invoke: fixture.invoke, isTauri: () => true }))
let root: Root
let host: HTMLDivElement
const props = { open: true, onOpenChange: vi.fn(), projectId: 'project-test', revision: 7, projectName: '测试项目', projectPath: 'C:/exports/测试项目.kaicut' }
let preflight: ExportPreflightResult
let record: ExportJobRecord
const button = (label: string) => Array.from(document.querySelectorAll('button')).find((item) => item.textContent === label)!
const click = async (label: string) => { await act(async () => button(label).click()) }
const render = async (overrides = {}) => { await act(async () => root.render(<ExportPreflightDialog {...props} {...overrides} />)) }
const deferred = <T,>() => {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((done) => { resolve = done })
  return { promise, resolve }
}

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true })
  props.onOpenChange.mockClear()
  preflight = { ready: true, projectId: props.projectId, revision: 7, outputPath: 'C:/exports/测试项目.mp4', durationFrames: 90,
    snapshot: createEmptyProject(), encoder: { available: true, name: 'h264_mf + aac', h264Mf: true, aac: true, detail: '编译列表通过' }, audio: { sampleRate: 48000, channels: 2 },
    checks: ['timeline', 'media', 'ffmpeg', 'audio', 'output'].map((id) => ({ id, label: id, state: 'pass' })) }
  record = { jobId: 'job-test', projectId: props.projectId, revision: 7, outputPath: preflight.outputPath, snapshotPath: 'C:/appdata/job-test/snapshot.json', state: 'queued', progress: 0, cancelRequested: false, retryAvailable: true, createdAt: '2026-10-11T00:00:00Z', updatedAt: '2026-10-11T00:00:00Z' }
  fixture.invoke.mockReset().mockImplementation(async (command: string) => {
    if (command === 'export_job_list') return []
    if (command === 'export_preflight') return preflight
    if (command === 'export_snapshot_create') return record
    if (command === 'export_job_start') return { ...record, state: 'running' }
    if (command === 'export_job_cancel') return { ...record, state: 'running', cancelRequested: true }
    if (command === 'export_job_status') return { ...record, state: 'completed', progress: 100 }
    if (command === 'export_choose_output') return preflight.outputPath
    throw new Error(`unexpected ${command}`)
  })
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
})
afterEach(async () => { await act(async () => root.unmount()); host.remove(); vi.useRealTimers() })

describe('export workflow', () => {
  it('shows recovered history without offering an unavailable retry', async () => {
    fixture.invoke.mockResolvedValue([{ ...record, state: 'failed', retryAvailable: false, error: '上次运行失败' }])
    await render()
    expect(document.body.textContent).toContain('上次运行失败')
    expect(button('重试导出')).toBeUndefined()
    expect(button('完成')).toBeDefined()
    await click('使用当前剪辑新建导出')
    expect(button('检查导出条件')).toBeDefined()
  })
  it('lets an unsaved project choose a native output path and checks it before encoding', async () => {
    await render({ projectPath: null })
    expect(button('检查导出条件').disabled).toBe(true)
    expect(button('开始导出').disabled).toBe(true)
    await click('选择位置…')
    expect(document.querySelector<HTMLInputElement>('[aria-label="导出位置"]')!.value).toBe(preflight.outputPath)
    await click('检查导出条件')
    expect(fixture.invoke).toHaveBeenCalledWith('export_preflight', { projectId: props.projectId, revision: 7, outputPath: preflight.outputPath, overwrite: false })
    expect(button('开始导出').disabled).toBe(false)
    expect(fixture.invoke.mock.calls.map(([name]) => name)).not.toContain('export_job_start')
  })
  it('blocks failed checks, remains dismissible, and reports errors', async () => {
    preflight.ready = false
    preflight.checks[1] = { id: 'media', label: '素材链接', state: 'fail', detail: '找不到原始素材' }
    await render(); await click('检查导出条件')
    expect(document.body.textContent).toContain('找不到原始素材')
    expect(button('开始导出').disabled).toBe(true)
    await click('关闭'); expect(props.onOpenChange).toHaveBeenCalledWith(false)
  })
  it('starts the exact snapshot once and polls actual progress to completion while editing continues', async () => {
    vi.useFakeTimers()
    await render(); await click('检查导出条件'); await click('开始导出')
    expect(fixture.invoke).toHaveBeenCalledWith('export_snapshot_create', { projectId: props.projectId, revision: 7, outputPath: preflight.outputPath, overwrite: false, preflight })
    expect(fixture.invoke).toHaveBeenCalledWith('export_job_start', { jobId: 'job-test' })
    expect(document.body.textContent).toContain('正在导出')
    await render({ revision: 8, open: false }); await render({ revision: 8 })
    await act(async () => vi.advanceTimersByTimeAsync(500))
    expect(document.body.textContent).toContain('导出完成')
    expect(document.querySelector('[role="progressbar"]')?.getAttribute('aria-valuenow')).toBe('100')
    expect(fixture.invoke.mock.calls.filter(([name]) => name === 'export_snapshot_create')).toHaveLength(1)
  })
  it('requests cancellation and waits for worker acknowledgement before saying cancelled', async () => {
    vi.useFakeTimers()
    const defaultImpl = fixture.invoke.getMockImplementation()!
    await render(); await click('检查导出条件'); await click('开始导出'); await click('取消导出')
    expect(fixture.invoke).toHaveBeenCalledWith('export_job_cancel', { jobId: 'job-test' })
    expect(document.body.textContent).toContain('正在取消')
    fixture.invoke.mockImplementation(async () => ({ ...record, state: 'cancelled', cancelRequested: false }))
    await act(async () => vi.advanceTimersByTimeAsync(500))
    expect(document.body.textContent).toContain('已取消')
    expect(button('重试导出')).toBeDefined()
    fixture.invoke.mockImplementation(defaultImpl)
    await render({ revision: 8 }); await click('重试导出')
    expect(document.body.textContent).toContain('正在导出')
    expect(fixture.invoke.mock.calls.filter(([name]) => name === 'export_snapshot_create')).toHaveLength(1)
  })
  it('does not reuse a preflight from an older revision', async () => {
    let resolve!: (value: ExportPreflightResult) => void
    const pending = new Promise<ExportPreflightResult>((done) => { resolve = done })
    fixture.invoke.mockImplementation(async (command) => command === 'export_job_list' ? [] : pending)
    await render(); await click('检查导出条件'); await render({ revision: 8 })
    await act(async () => resolve(preflight))
    expect(button('开始导出').disabled).toBe(true)
    expect(document.body.textContent).not.toContain('检查完成')
  })
  it('shows start failures and retries the same snapshot rather than creating another job', async () => {
    const defaultImpl = fixture.invoke.getMockImplementation()!
    fixture.invoke.mockImplementation(async (command, args) => {
      if (command === 'export_job_start') throw new Error('当前设备无法启动编码器')
      return defaultImpl(command, args)
    })
    await render(); await click('检查导出条件'); await click('开始导出')
    expect(document.body.textContent).toContain('当前设备无法启动编码器')
    fixture.invoke.mockImplementation(defaultImpl)
    await click('重试导出')
    expect(document.body.textContent).toContain('正在导出')
    expect(fixture.invoke.mock.calls.filter(([name]) => name === 'export_snapshot_create')).toHaveLength(1)
  })
  it('does not restore dismissed history when a pending list returns after choosing a new export', async () => {
    const pending = deferred<ExportJobRecord[]>()
    const history = { ...record, state: 'completed' as const, progress: 100, retryAvailable: false }
    fixture.invoke.mockResolvedValueOnce([history]).mockReturnValueOnce(pending.promise)
    await render(); await render({ open: false }); await render()
    await click('使用当前剪辑新建导出')
    await act(async () => pending.resolve([history]))
    expect(button('检查导出条件')).toBeDefined()
    expect(button('完成')).toBeUndefined()
  })
  it('shows the new project export form when the previous project has a completed job', async () => {
    const defaultImpl = fixture.invoke.getMockImplementation()!
    fixture.invoke.mockResolvedValueOnce([{ ...record, state: 'completed', progress: 100, retryAvailable: false }])
    await render()
    fixture.invoke.mockImplementation(defaultImpl)
    await render({ projectId: 'next-project', projectName: '下个项目', projectPath: 'C:/exports/next.kaicut' })
    expect(button('检查导出条件')).toBeDefined()
    expect(document.querySelector<HTMLInputElement>('[aria-label="导出位置"]')?.value).toBe('C:/exports/next.mp4')
    expect(document.body.textContent).not.toContain('导出完成')
  })
  it('retains a running previous-project job until its terminal status arrives', async () => {
    vi.useFakeTimers()
    await render(); await click('检查导出条件'); await click('开始导出')
    await render({ projectId: 'next-project', projectPath: 'C:/exports/next.kaicut' })
    expect(button('取消导出')).toBeDefined()
    expect(document.body.textContent).toContain(record.outputPath)
    await act(async () => vi.advanceTimersByTimeAsync(500))
    expect(button('检查导出条件')).toBeDefined()
    expect(document.querySelector<HTMLInputElement>('[aria-label="导出位置"]')?.value).toBe('C:/exports/next.mp4')
  })
  it('does not revive a previous-project running job from history older than the latest status poll', async () => {
    vi.useFakeTimers()
    const pending = deferred<ExportJobRecord[]>()
    const defaultImpl = fixture.invoke.getMockImplementation()!
    await render(); await click('检查导出条件'); await click('开始导出')
    fixture.invoke.mockImplementation(async (command, args) => command === 'export_job_list' ? pending.promise : defaultImpl(command, args))
    await render({ projectId: 'next-project', projectPath: 'C:/exports/next.kaicut' })
    await act(async () => vi.advanceTimersByTimeAsync(500))
    await act(async () => pending.resolve([{ ...record, state: 'running' }]))
    expect(button('检查导出条件')).toBeDefined()
    expect(button('取消导出')).toBeUndefined()
  })
  it('lets the new project check independently without accepting or clearing a newer check with stale results', async () => {
    const oldCheck = deferred<ExportPreflightResult>()
    const newCheck = deferred<ExportPreflightResult>()
    const nextPreflight = { ...preflight, projectId: 'next-project', outputPath: 'C:/exports/next.mp4' }
    fixture.invoke.mockImplementation(async (command, args) => {
      if (command === 'export_job_list') return []
      if (command === 'export_preflight') return args.projectId === props.projectId ? oldCheck.promise : newCheck.promise
      throw new Error(`unexpected ${command}`)
    })
    await render(); await click('检查导出条件')
    await render({ projectId: 'next-project', projectPath: 'C:/exports/next.kaicut' })
    expect(button('检查导出条件').disabled).toBe(false)
    await click('检查导出条件')
    await act(async () => oldCheck.resolve(preflight))
    expect(button('检查导出条件').disabled).toBe(true)
    expect(button('开始导出').disabled).toBe(true)
    await act(async () => newCheck.resolve(nextPreflight))
    expect(button('开始导出').disabled).toBe(false)
  })
  it('clears a transient polling failure after progress recovers', async () => {
    vi.useFakeTimers()
    const defaultImpl = fixture.invoke.getMockImplementation()!
    let failed = false
    fixture.invoke.mockImplementation(async (command, args) => {
      if (command === 'export_job_status' && !failed) { failed = true; throw new Error('暂时无法连接') }
      return defaultImpl(command, args)
    })
    await render(); await click('检查导出条件'); await click('开始导出')
    await act(async () => vi.advanceTimersByTimeAsync(500))
    expect(document.body.textContent).toContain('读取导出进度失败')
    await act(async () => vi.advanceTimersByTimeAsync(500))
    expect(document.body.textContent).toContain('导出完成')
    expect(document.querySelector('[role="alert"]')).toBeNull()
  })
  it('keeps an unrelated cancel error when a subsequent status poll succeeds', async () => {
    vi.useFakeTimers()
    const defaultImpl = fixture.invoke.getMockImplementation()!
    fixture.invoke.mockImplementation(async (command, args) => {
      if (command === 'export_job_cancel') throw new Error('取消请求未送达')
      if (command === 'export_job_status') return { ...record, state: 'running', progress: 25 }
      return defaultImpl(command, args)
    })
    await render(); await click('检查导出条件'); await click('开始导出'); await click('取消导出')
    await act(async () => vi.advanceTimersByTimeAsync(500))
    expect(document.body.textContent).toContain('取消请求未送达')
    expect(document.querySelector('[role="progressbar"]')?.getAttribute('aria-valuenow')).toBe('25')
    expect(button('取消导出').disabled).toBe(false)
  })
})
