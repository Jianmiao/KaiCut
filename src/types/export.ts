import type { KaicutProject } from '@/types/project'

export type ExportCheckState = 'pass' | 'warn' | 'fail'
export type ExportJobState = 'queued' | 'running' | 'completed' | 'cancelled' | 'failed' | 'interrupted'

export interface ExportPreflightCheck {
  id: string
  label: string
  state: ExportCheckState
  detail?: string
}

/**
 * The preflight response is deliberately separate from the export job API.
 * A successful preflight only permits creating a queued immutable snapshot;
 * neither the preflight nor snapshot creation starts an encoder or writes the
 * requested output file.
 */
export interface ExportPreflightResult {
  ready: boolean
  projectId: string
  revision: number
  outputPath: string
  durationFrames: number
  snapshot: KaicutProject
  encoder: {
    available: boolean
    name: string
    h264Mf: boolean
    aac: boolean
    detail: string
  }
  checks: ExportPreflightCheck[]
  audio: {
    sampleRate: number
    channels: number
  }
}

export interface ExportJobRecord {
  jobId: string
  projectId: string
  revision: number
  snapshotPath: string
  outputPath: string
  state: ExportJobState
  progress: number
  cancelRequested: boolean
  retryAvailable: boolean
  createdAt: string
  updatedAt: string
  error?: string
}
