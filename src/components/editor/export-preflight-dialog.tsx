import { useEffect, useRef, useState } from 'react'
import { AlertCircle, CheckCircle2, CircleX, Download } from 'lucide-react'
import { invoke, isTauri } from '@tauri-apps/api/core'
import { Dialog } from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Progress } from '@/components/ui/progress'
import type { ExportJobRecord, ExportPreflightResult } from '@/types/export'

interface Props {
  open: boolean
  onOpenChange: (open: boolean) => void
  projectId: string
  revision: number
  projectName: string
  projectPath: string | null
}

const stateLabels = { queued: '等待开始', running: '正在导出', completed: '导出完成', cancelled: '已取消', failed: '导出失败', interrupted: '上次导出已中断' }
const message = (error: unknown) => error instanceof Error ? error.message : String(error)

export function ExportPreflightDialog({ open, onOpenChange, projectId, revision, projectName, projectPath }: Props) {
  const suggestedPath = projectPath?.replace(/\.kaicut$/i, '.mp4') ?? ''
  const [pathDraft, setPathDraft] = useState<{ projectId: string; path: string } | null>(null)
  const outputPath = pathDraft?.projectId === projectId ? pathDraft.path : suggestedPath
  const [result, setResult] = useState<ExportPreflightResult | null>(null)
  const [checking, setChecking] = useState(false)
  const [actionBusy, setActionBusy] = useState(false)
  const [error, setError] = useState('')
  const [pollError, setPollError] = useState<{ jobId: string; message: string } | null>(null)
  const [storedJob, setJob] = useState<ExportJobRecord | null>(null)
  const job = storedJob?.projectId === projectId || storedJob?.state === 'running' ? storedJob : null
  const request = useRef(0)
  const historyRequest = useRef(0)
  const mounted = useRef(false)
  const contextKey = `${projectId}:${revision}:${outputPath}:${open}`
  const [checkingContext, setCheckingContext] = useState(contextKey)
  // A slow check for another project must not block this project's form.
  if (checkingContext !== contextKey) {
    setCheckingContext(contextKey)
    setChecking(false)
  }
  useEffect(() => { mounted.current = true; return () => { mounted.current = false } }, [])
  useEffect(() => { request.current++; }, [contextKey, open])
  const currentResult = result?.projectId === projectId && result.revision === revision && result.outputPath === outputPath ? result : null
  const ready = !!currentResult?.ready && !currentResult.checks.some((check) => check.state === 'fail')
  const running = job?.state === 'running'
  const canRetry = job?.retryAvailable && ['queued', 'cancelled', 'failed'].includes(job.state)

  // The dialog stays mounted when hidden, so closing it does not lose the task.
  useEffect(() => {
    if (!open || !isTauri()) return
    let active = true
    const generation = ++historyRequest.current
    void invoke<ExportJobRecord[]>('export_job_list').then((jobs) => {
      if (!active || generation !== historyRequest.current || !Array.isArray(jobs)) return
      const ongoing = jobs.find((item) => item.state === 'running')
      const recent = jobs.filter((item) => item.projectId === projectId).sort((a, b) => b.createdAt.localeCompare(a.createdAt))[0]
      setJob((current) => current?.state === 'running' || current?.projectId === projectId ? current : ongoing ?? recent ?? null)
    }).catch(() => { /* Preflight remains usable if old job metadata is unavailable. */ })
    return () => { active = false }
  }, [open, projectId])
  const jobId = job?.jobId
  useEffect(() => {
    if (!jobId || !running) return
    let active = true
    let timer: number | undefined
    const poll = async () => {
      try {
        const next = await invoke<ExportJobRecord>('export_job_status', { jobId })
        if (!active || next.jobId !== jobId) return
        historyRequest.current++
        setJob(next)
        setPollError(null)
        if (next.state !== 'running') return
      } catch (reason) { if (active) setPollError({ jobId, message: `读取导出进度失败：${message(reason)}` }) }
      if (active) timer = window.setTimeout(() => void poll(), 500)
    }
    timer = window.setTimeout(() => void poll(), 500)
    return () => { active = false; window.clearTimeout(timer) }
  }, [jobId, running])

  const chooseOutput = async () => {
    historyRequest.current++
    setActionBusy(true); setError('')
    try {
      const path = await invoke<string | null>('export_choose_output')
      if (path && mounted.current) { request.current++; setPathDraft({ projectId, path }); setResult(null) }
    } catch (reason) { if (mounted.current) setError(message(reason)) }
    finally { if (mounted.current) setActionBusy(false) }
  }
  const check = async () => {
    const generation = ++request.current
    historyRequest.current++
    setChecking(true); setError(''); setResult(null)
    try {
      const response = await invoke<ExportPreflightResult>('export_preflight', { projectId, revision, outputPath, overwrite: false })
      if (mounted.current && generation === request.current) setResult(response)
    } catch (reason) { if (mounted.current && generation === request.current) setError(message(reason)) }
    finally { if (mounted.current && generation === request.current) setChecking(false) }
  }
  const start = async () => {
    if (actionBusy || running || (!canRetry && !ready)) return false
    setActionBusy(true); setError('')
    try {
      let created = job
      if (!created && currentResult) {
        created = await invoke<ExportJobRecord>('export_snapshot_create', {
          projectId: currentResult.projectId, revision: currentResult.revision,
          outputPath: currentResult.outputPath, overwrite: false, preflight: currentResult,
        })
        if (mounted.current) setJob(created)
      }
      if (created) {
        const started = await invoke<ExportJobRecord>('export_job_start', { jobId: created.jobId })
        if (mounted.current) setJob(started)
      }
    } catch (reason) { if (mounted.current) setError(message(reason)) }
    finally { if (mounted.current) setActionBusy(false) }
    return false
  }
  const cancel = async () => {
    if (!job || actionBusy) return
    setActionBusy(true); setError('')
    try {
      const next = await invoke<ExportJobRecord>('export_job_cancel', { jobId: job.jobId })
      if (mounted.current) setJob(next)
    } catch (reason) { if (mounted.current) setError(message(reason)) }
    finally { if (mounted.current) setActionBusy(false) }
  }
  const terminal = job && !running && !canRetry
  const status = job ? job.cancelRequested ? '正在取消…' : stateLabels[job.state] : checking ? '正在检查…' : currentResult ? ready ? '检查完成' : '请先处理以下问题' : '选择位置后检查导出条件'
  return <Dialog open={open} onOpenChange={onOpenChange} title="导出视频"
    description="MP4 · 1080p · H.264 / AAC · 使用项目帧率" icon={<Download size={18} strokeWidth={1.6} />}
    confirmText={terminal ? '完成' : canRetry ? '重试导出' : '开始导出'} cancelText={running ? '继续编辑' : '关闭'}
    hideConfirm={running} confirmDisabled={!terminal && (checking || actionBusy || (!ready && !canRetry))}
    onConfirm={terminal ? () => true : start}>
    <div className="export-preflight">
      {!job && <>
        <label className="export-preflight-output">保存到
          <Input aria-label="导出位置" value={outputPath} disabled={actionBusy || checking}
            placeholder="选择 MP4 保存位置" onChange={(event) => { request.current++; historyRequest.current++; setPathDraft({ projectId, path: event.target.value }); setResult(null) }} />
        </label>
        <div className="flex justify-end gap-2">
          <Button variant="secondary" size="sm" disabled={actionBusy || checking || !isTauri()} onClick={() => void chooseOutput()}>选择位置…</Button>
          <Button variant="secondary" size="sm" disabled={actionBusy || checking || !outputPath || !isTauri()} onClick={() => void check()}>检查导出条件</Button>
        </div>
        <p className="export-preflight-note">{projectName} · 最多 64 个片段。已有文件请换名保存。</p>
        {!isTauri() && <p role="status">请在开剪桌面应用中导出视频。</p>}
      </>}
      <p className="export-preflight-status" role="status">{status}</p>
      {!job && currentResult && <ul className="export-preflight-list" aria-label="导出检查项目">
        {currentResult.checks.map((check) => <li className={`export-preflight-item is-${check.state}`} key={check.id}>
          <span className="export-preflight-icon" aria-hidden="true">{check.state === 'pass' ? <CheckCircle2 size={16} /> : check.state === 'warn' ? <AlertCircle size={16} /> : <CircleX size={16} />}</span>
          <span className="export-preflight-copy"><strong>{check.label}</strong><small>{check.detail}</small></span>
        </li>)}
      </ul>}
      {job && <div className="export-preflight-job">
        <Progress value={job.progress} aria-label="视频导出进度" aria-valuetext={`${Math.floor(job.progress)}%`} />
        <span>{Math.floor(job.progress)}%</span><span className="source-path">{job.outputPath}</span>
        {job.error && <p role="alert">{job.error}</p>}
        {running && <Button variant="secondary" disabled={actionBusy || job.cancelRequested} onClick={() => void cancel()}>取消导出</Button>}
        {!running && <Button variant="secondary" disabled={actionBusy} onClick={() => { historyRequest.current++; request.current++; setJob(null); setResult(null); setError(''); setPollError(null) }}>使用当前剪辑新建导出</Button>}
      </div>}
      {error && <p className="export-preflight-error" role="alert">{error}</p>}
      {job && pollError?.jobId === job.jobId && <p className="export-preflight-error" role="alert">{pollError.message}</p>}
      <p className="export-preflight-note">开始后可继续编辑。取消或失败会清理未完成的视频，原素材保持不变。</p>
    </div>
  </Dialog>
}
