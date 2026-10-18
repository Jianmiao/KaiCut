import { useEffect } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { isDesktop, useProjectStore } from '@/stores/project-store'
import type { NativeSession } from '@/types/project'
import type { TransportRequest } from '@/components/editor/preview-area'

interface McpCommand {
  requestId: string
  action: 'inspect' | 'seek' | 'play' | 'pause' | 'select'
  args: { projectId?: string; revision?: number; frame?: number; clipIds?: string[] }
}

const finiteOrNull = (value: number) => Number.isFinite(value) ? value : null
const counterOrNull = (value?: string) => {
  if (!value || !/^\d+$/.test(value)) return null
  const number = Number(value)
  return Number.isSafeInteger(number) ? number : null
}

/** Bounded semantic view of the real rendered application, without eval or screenshots. */
export function inspectRenderedUi() {
  const inspectionStartedAt = performance.now()
  const state = useProjectStore.getState()
  const preview = document.querySelector<HTMLElement>('[data-preview-mode]')
  const clock = preview?.dataset.previewClock
  const clockMode = clock === 'audio' || clock === 'video' || clock === 'monotonic' || clock === 'source' ? clock : null
  const observedPhase = preview?.dataset.previewPhase
  const phase = observedPhase === 'preparing' || observedPhase === 'starting' || observedPhase === 'playing'
    || observedPhase === 'buffering' || observedPhase === 'paused' || observedPhase === 'source' ? observedPhase : null
  const buffering = preview?.dataset.previewBuffering
  const controls = Array.from(document.querySelectorAll<HTMLElement>('button,input,[role="slider"],[role="separator"]'))
    .filter((element) => !element.closest('[hidden], [aria-hidden="true"]') && element.getClientRects().length > 0)
    .slice(0, 200).map((element) => {
      const rect = element.getBoundingClientRect()
      const style = getComputedStyle(element)
      return { role: element.getAttribute('role') ?? element.tagName.toLowerCase(),
        name: (element.getAttribute('aria-label') ?? element.textContent ?? '').trim().slice(0, 160),
        disabled: element.matches(':disabled'), pressed: element.getAttribute('aria-pressed'),
        bounds: { x: Math.round(rect.x), y: Math.round(rect.y), width: Math.round(rect.width), height: Math.round(rect.height) },
        style: { fontFamily: style.fontFamily, fontSize: style.fontSize, color: style.color, background: style.backgroundColor, borderRadius: style.borderRadius } }
    })
  const mediaNodes = Array.from(document.querySelectorAll<HTMLMediaElement>('video,audio')).slice(0, 4).map((element) => {
    const clipId = element.dataset.previewClipId ?? null
    const clip = state.previewMode === 'timeline'
      ? state.project.tracks.flatMap((track) => track.clips).find((candidate) => candidate.id === clipId)
      : undefined
    const { fpsNumerator, fpsDenominator } = state.project.timebase
    const expectedSourceTime = clip && fpsNumerator > 0 && fpsDenominator > 0
      ? finiteOrNull((clip.sourceInFrame + state.playheadFrame - clip.startFrame) * fpsDenominator / fpsNumerator)
      : null
    const currentTime = finiteOrNull(element.currentTime)
    let quality: VideoPlaybackQuality | null = null
    if (element instanceof HTMLVideoElement && typeof element.getVideoPlaybackQuality === 'function') {
      try { quality = element.getVideoPlaybackQuality() } catch { /* Unsupported by this WebView. */ }
    }
    return {
      mediaId: element.dataset.previewMediaId ?? null, clipId, nodeId: element.dataset.previewNodeId || null,
      kind: element.tagName.toLowerCase(), currentTime, paused: element.paused,
      playbackRate: finiteOrNull(element.playbackRate), hardSeekCount: counterOrNull(element.dataset.previewHardSeeks),
      volume: element.volume, muted: element.muted, readyState: element.readyState,
      seeking: element.seeking, errorCode: element.error?.code ?? null,
      expectedSourceTime,
      driftSeconds: currentTime !== null && expectedSourceTime !== null ? finiteOrNull(currentTime - expectedSourceTime) : null,
      totalVideoFrames: quality ? finiteOrNull(quality.totalVideoFrames) : null,
      droppedVideoFrames: quality ? finiteOrNull(quality.droppedVideoFrames) : null,
    }
  })
  const imageNodes = Array.from(document.querySelectorAll<HTMLImageElement>('.preview-canvas img')).slice(0, 2).map((element) => ({
    complete: element.complete, naturalWidth: element.naturalWidth, naturalHeight: element.naturalHeight,
  }))
  const alerts = Array.from(document.querySelectorAll('[role="alert"]')).map((item) => item.textContent?.slice(0, 300)).slice(0, 10)
  const domNodeCount = document.querySelectorAll('*').length
  const timelineClipElementCount = document.querySelectorAll('.timeline-clip').length
  const inspectionEndedAt = performance.now()
  return { capturedAt: new Date().toISOString(), projectId: state.project.projectId, revision: state.revision,
    busy: state.busy, dirty: state.isDirty, selectedClipIds: state.selectedClipIds, selectedMediaId: state.selectedMediaId,
    playheadFrame: state.playheadFrame, preview: { mode: state.previewMode, playing: preview?.dataset.playing === 'true',
      phase, clockMode, masterKind: clockMode === 'audio' || clockMode === 'video' ? clockMode : null,
      masterClipId: preview?.dataset.previewClockClipId || null,
      buffering: buffering === 'true' ? true : buffering === 'false' ? false : null,
      visualClipId: preview?.dataset.previewVisual || null, audioClipIds: preview?.dataset.previewAudio?.split(',').filter(Boolean) ?? [] },
    alerts,
    viewport: { width: innerWidth, height: innerHeight, visibility: document.visibilityState, hidden: document.hidden }, controls, mediaNodes, imageNodes,
    performance: {
      inspectedAtMonotonicMs: finiteOrNull(inspectionStartedAt),
      inspectionDurationMs: finiteOrNull(Math.max(0, inspectionEndedAt - inspectionStartedAt)),
      domNodeCount, timelineClipElementCount,
    },
    evidence: 'semantic DOM and runtime state; not a screenshot or proof of decoded pixels',
  }
}

export function useMcpBridge() {
  useEffect(() => {
    if (!isDesktop) return
    let active = true
    const cleanup: Array<() => void> = []
    const subscribe = (subscription: Promise<() => void>) => {
      void subscription.then((unlisten) => { if (active) cleanup.push(unlisten); else unlisten() }).catch(() => { /* MCP is optional. */ })
    }
    subscribe(listen<NativeSession>('kaicut:mcp-session', ({ payload }) => {
      if (active) useProjectStore.getState().applyNativeSession(payload)
    }))
    subscribe(listen<McpCommand>('kaicut:mcp-command', ({ payload }) => {
      void (async () => {
        let result: object
        try {
          if (!active) return
          const { action, args } = payload
          const current = useProjectStore.getState()
          if (action !== 'inspect' && (args.projectId !== current.project.projectId || args.revision !== current.revision)) throw new Error('界面项目已更新，请重新读取状态')
          if (action !== 'inspect' && current.busy) throw new Error('项目正在处理中，请稍后重试')
          if (action !== 'inspect' && document.querySelector('[aria-modal="true"]')) throw new Error('请先关闭当前对话框再执行界面控制')
          if (action === 'seek') {
            if (!Number.isSafeInteger(args.frame) || args.frame! < 0) throw new Error('定位帧无效')
            current.setPlayheadFrame(args.frame!)
          } else if (action === 'select') current.selectClips(args.clipIds ?? [])
          else if (action === 'play' || action === 'pause') {
            if (action === 'play') current.setPreviewMode('timeline')
            await new Promise((resolve) => window.setTimeout(resolve, 20))
            const latest = useProjectStore.getState()
            if (args.projectId !== latest.project.projectId || args.revision !== latest.revision) throw new Error('界面项目已更新，请重新读取状态')
            await new Promise<void>((resolve, reject) => {
              const timer = window.setTimeout(() => reject(new Error('播放器尚未就绪')), 500)
              const detail: TransportRequest = { action, resolve: () => { clearTimeout(timer); resolve() }, reject: (error) => { clearTimeout(timer); reject(error) } }
              window.dispatchEvent(new CustomEvent('kaicut:transport', { detail }))
            })
          }
          await new Promise((resolve) => window.setTimeout(resolve, 32))
          result = inspectRenderedUi()
        } catch (error) { result = { error: error instanceof Error ? error.message : String(error) } }
        if (active) await invoke('mcp_ui_ack', { requestId: payload.requestId, result }).catch(() => { /* Request timed out or client disconnected. */ })
      })()
    }))
    return () => { active = false; cleanup.forEach((unlisten) => unlisten()) }
  }, [])
}
