import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { createHash } from 'node:crypto'
import { readFile, readdir, stat, writeFile } from 'node:fs/promises'
import { dirname, join } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'
import { promisify } from 'node:util'

const runFile = promisify(execFile)
const terminal = new Set(['completed', 'cancelled', 'failed', 'interrupted'])

async function exists(path) {
  try { await stat(path); return true } catch (error) {
    if (error.code === 'ENOENT') return false
    throw error
  }
}

async function digest(path) {
  return createHash('sha256').update(await readFile(path)).digest('hex')
}

// Fixed diagnostic commands only. Paths stay individual argv entries, never shell text.
async function runSidecar(binary, args) {
  const { stdout } = await runFile(binary, args, { windowsHide: true, timeout: 30000, maxBuffer: 16 * 1024 * 1024, encoding: 'buffer' })
  return stdout
}

function wavRms(bytes, start, end) {
  assert.equal(bytes.toString('ascii', 0, 4), 'RIFF')
  assert.equal(bytes.toString('ascii', 8, 12), 'WAVE')
  let format
  let pcm
  for (let offset = 12; offset + 8 <= bytes.length;) {
    const size = bytes.readUInt32LE(offset + 4)
    assert.ok(offset + 8 + size <= bytes.length, 'WAV chunk is truncated')
    const name = bytes.toString('ascii', offset, offset + 4)
    if (name === 'fmt ') {
      assert.ok(size >= 16)
      format = { encoding: bytes.readUInt16LE(offset + 8), channels: bytes.readUInt16LE(offset + 10), rate: bytes.readUInt32LE(offset + 12), bits: bytes.readUInt16LE(offset + 22) }
    }
    if (name === 'data') pcm = bytes.subarray(offset + 8, offset + 8 + size)
    offset += 8 + size + size % 2
  }
  assert.deepEqual(format, { encoding: 1, channels: 2, rate: 48000, bits: 16 })
  assert.ok(pcm, 'PCM data chunk is required')
  const from = Math.floor(start * format.rate)
  const to = Math.floor(end * format.rate)
  assert.ok(to > from && to * 4 <= pcm.length)
  let energy = 0
  for (let i = from; i < to; i++) energy += (pcm.readInt16LE(i * 4) / 32767) ** 2
  return Math.sqrt(energy / (to - from))
}

async function decodeOutput(workspace, directory, outputPath) {
  const ffmpeg = join(workspace, 'src-tauri', 'binaries', 'ffmpeg-x86_64-pc-windows-msvc.exe')
  const ffprobe = join(workspace, 'src-tauri', 'binaries', 'ffprobe-x86_64-pc-windows-msvc.exe')
  const probe = JSON.parse((await runSidecar(ffprobe, ['-v', 'error', '-protocol_whitelist', 'file', '-show_streams', '-show_format', '-of', 'json', outputPath])).toString('utf8'))
  const video = probe.streams.find((stream) => stream.codec_type === 'video')
  const audio = probe.streams.find((stream) => stream.codec_type === 'audio')
  assert.ok(video && audio)
  assert.equal(video.codec_name, 'h264')
  assert.equal(video.width, 1920)
  assert.equal(video.height, 1080)
  assert.equal(video.avg_frame_rate, '30/1')
  assert.equal(Number(video.nb_frames), 120)
  assert.ok(Math.abs(Number(probe.format.duration) - 4) < 0.1)
  assert.equal(audio.codec_name, 'aac')
  assert.equal(Number(audio.sample_rate), 48000)
  assert.equal(audio.channels, 2)
  await writeFile(join(directory, 'export-probe.json'), `${JSON.stringify(probe, null, 2)}\n`, { flag: 'wx' })

  const rgbPath = join(directory, 'export-decoded.rgb')
  await runSidecar(ffmpeg, ['-v', 'error', '-nostdin', '-n', '-protocol_whitelist', 'file', '-i', outputPath, '-map', '0:v:0', '-vf', 'scale=8:8,format=rgb24', '-c:v', 'rawvideo', '-f', 'rawvideo', rgbPath])
  const rgb = await readFile(rgbPath)
  assert.equal(rgb.length, 120 * 8 * 8 * 3)
  const samples = []
  for (const [frame, color] of [[15, 'blue'], [45, 'black'], [75, 'blue'], [105, 'yellow']]) {
    const pixels = rgb.subarray(frame * 192, (frame + 1) * 192)
    for (let i = 0; i < pixels.length; i += 3) {
      const [red, green, blue] = pixels.subarray(i, i + 3)
      assert.ok(color === 'blue' ? blue > 180 && red < 35 && green < 35
        : color === 'black' ? Math.max(red, green, blue) < 12
          : red > 180 && green > 180 && blue < 35, `Frame ${frame} must decode as ${color}`)
    }
    samples.push({ frame, expected: color, rgb: Array.from(pixels.subarray(0, 3)) })
  }
  const wavPath = join(directory, 'export-decoded.wav')
  await runSidecar(ffmpeg, ['-v', 'error', '-nostdin', '-n', '-protocol_whitelist', 'file', '-i', outputPath, '-map', '0:a:0', '-c:a', 'pcm_s16le', wavPath])
  const wav = await readFile(wavPath)
  const rms = { silence: wavRms(wav, 0.1, 0.8), full: wavRms(wav, 2.1, 2.7), fadeOut: wavRms(wav, 3.68, 3.72) }
  assert.ok(rms.silence < 0.0005, 'Generated leading silence must remain silent')
  // Source sine amplitude .4, mono->stereo gain sqrt(.5), frozen track gain .5.
  assert.ok(Math.abs(rms.full - 0.1) < 0.015, 'Output must use frozen gain .5, not the later live gain .25')
  assert.ok(Math.abs(rms.fadeOut / rms.full - (4 - 1 / 30 - 3.7) / 0.5) < 0.09, 'Audio fade-out must match the frame-based project fade')
  return { outputPath, sha256: await digest(outputPath), bytes: (await stat(outputPath)).size,
    video: { codec: video.codec_name, width: video.width, height: video.height, fps: video.avg_frame_rate, frames: Number(video.nb_frames) },
    audio: { codec: audio.codec_name, sampleRate: Number(audio.sample_rate), channels: audio.channels }, samples, rms }
}

/** Runs only after workflow-check has created its own generated, saved test project. */
export async function validateExportWorkflow({ call, update, context, runDirectory, workspace, report }) {
  assert.equal(process.platform, 'win32', 'This export preset requires Windows Media Foundation')
  const ownedJobs = new Set()
  const requestedOutputs = new Set()
  const evidence = { checks: [], jobs: [] }
  let workflowError
  const cleanupErrors = []
  report.export = evidence
  const check = (description) => evidence.checks.push(description)
  const stateBefore = await call('get_state')
  const jobsBefore = await call('export_job_list')
  assert.equal(jobsBefore.length, 0, 'Export validation requires an isolated app with no pre-existing jobs')
  const jobStatus = (jobId) => call('export_job_status', { jobId })
  const waitForJob = async (jobId, predicate, description, timeoutMs = 60000) => {
    const deadline = Date.now() + timeoutMs
    while (Date.now() < deadline) {
      const job = await jobStatus(jobId)
      if (predicate(job)) return job
      if (terminal.has(job.state)) throw new Error(`${description}: unexpected terminal state ${job.state}: ${job.error || ''}`)
      await delay(100)
    }
    throw new Error(`${description}: deadline exceeded; inspect task before retrying`)
  }
  const prepare = async (outputPath) => {
    requestedOutputs.add(outputPath)
    const job = await call('export_prepare', { ...context(), outputPath })
    ownedJobs.add(job.jobId)
    assert.equal(job.state, 'queued')
    assert.equal(await exists(outputPath), false, 'Preparing a snapshot must not create the MP4')
    evidence.jobs.push({ jobId: job.jobId, outputPath })
    return job
  }
  const assertNoTemporary = async (jobId) => {
    assert.ok((await readdir(runDirectory)).every((name) => !name.startsWith(`.kaicut-export-${jobId}-`)), 'Task-owned temporary output must be removed')
    const job = await jobStatus(jobId)
    const snapshotDir = dirname(job.snapshotPath)
    assert.ok((await readdir(snapshotDir)).every((name) => !name.startsWith(`filter-${jobId}-`)), 'Task-owned filter script must be removed')
  }
  try {
    const outputPath = join(runDirectory, 'MCP 导出成片;[literal].mp4')
    await assert.rejects(call('export_prepare', { ...context(), revision: context().revision + 1, outputPath }), /export_prepare:/)
    assert.deepEqual(await call('export_job_list'), jobsBefore)
    assert.deepEqual(await call('get_state'), stateBefore)
    const preflight = await call('export_preflight', { ...context(), outputPath })
    assert.equal(preflight.ready, true, JSON.stringify(preflight.checks))
    assert.deepEqual(await call('export_job_list'), jobsBefore)
    assert.equal(await exists(outputPath), false)
    check('Preflight is read-only; stale preparation cannot create a job or file')

    const frozen = await prepare(outputPath)
    assert.deepEqual(await call('get_state'), stateBefore)
    const audioTrack = stateBefore.project.tracks.find((track) => track.kind === 'audio' && track.clips.length)
    assert.equal(audioTrack.volume, 0.5)
    await update('timeline_edit', { edit: { type: 'trackVolume', trackId: audioTrack.id, volume: 0.25 } })
    const afterEdit = await call('get_state')
    assert.notEqual(afterEdit.revision, frozen.revision)
    const started = await call('export_job_start', { jobId: frozen.jobId })
    assert.equal(started.state, 'running')
    const completed = await waitForJob(frozen.jobId, (job) => job.state === 'completed', 'Real export completion')
    assert.equal(completed.progress, 100)
    assert.deepEqual(await call('get_state'), afterEdit, 'Export must not mutate the current editing session')
    evidence.completed = completed
    evidence.decoded = await decodeOutput(workspace, runDirectory, outputPath)
    await assertNoTemporary(frozen.jobId)
    check('Frozen snapshot exports H264/AAC while the editing project advances independently')
    check('Decoded output has 120 frames, blue/black/blue/yellow sequence, expected gain and fade-out')
    await assert.rejects(call('export_job_start', { jobId: frozen.jobId }), /export_job_start:/)
    await assert.rejects(call('export_job_cancel', { jobId: frozen.jobId }), /export_job_cancel:/)
    assert.equal(await digest(outputPath), evidence.decoded.sha256)
    check('Completed output survives repeated start and late cancellation requests')

    const protectedPath = join(runDirectory, '禁止覆盖.mp4')
    const protectedJob = await prepare(protectedPath)
    await writeFile(protectedPath, 'KaiCut export must preserve this existing file.\n', { flag: 'wx' })
    const sentinelHash = await digest(protectedPath)
    const jobCount = (await call('export_job_list')).length
    const blocked = await call('export_preflight', { ...context(), outputPath: protectedPath })
    assert.equal(blocked.ready, false)
    await assert.rejects(call('export_prepare', { ...context(), outputPath: protectedPath }), /export_prepare:/)
    await assert.rejects(call('export_job_start', { jobId: protectedJob.jobId }), /export_job_start:/)
    assert.equal((await call('export_job_list')).length, jobCount)
    assert.equal((await jobStatus(protectedJob.jobId)).state, 'queued')
    assert.equal(await digest(protectedPath), sentinelHash)
    await call('export_job_cancel', { jobId: protectedJob.jobId })
    check('A destination created after preparation is preserved; preflight, prepare and start all refuse overwrite')

    const queuedPath = join(runDirectory, '取消排队.mp4')
    const queued = await prepare(queuedPath)
    assert.equal((await call('export_job_cancel', { jobId: queued.jobId })).state, 'cancelled')
    assert.equal((await call('export_job_cancel', { jobId: queued.jobId })).state, 'cancelled')
    assert.equal(await exists(queuedPath), false)
    await assertNoTemporary(queued.jobId)
    check('Queued cancellation is repeatable and creates no output')

    const imageClip = afterEdit.project.tracks.flatMap((track) => track.clips).find((clip) => afterEdit.project.mediaRefs.find((media) => media.id === clip.mediaId)?.kind === 'image')
    assert.ok(imageClip)
    await update('timeline_edit', { edit: { type: 'trim', clipId: imageClip.id, sourceInFrame: 0, startFrame: imageClip.startFrame, durationFrames: 900 } })
    const cancelPath = join(runDirectory, '取消运行.mp4')
    const runningJob = await prepare(cancelPath)
    const startedCancel = await call('export_job_start', { jobId: runningJob.jobId })
    assert.equal(startedCancel.state, 'running')
    await assert.rejects(call('export_job_start', { jobId: runningJob.jobId }), /export_job_start:/)
    const encoding = await waitForJob(runningJob.jobId, (job) => job.state === 'running' && job.progress > 0 && job.progress < 100, 'Observe actual encoder progress before cancelling', 30000)
    const cancelAcknowledged = await call('export_job_cancel', { jobId: runningJob.jobId })
    assert.ok(cancelAcknowledged.cancelRequested || cancelAcknowledged.state === 'cancelled')
    const cancelled = await waitForJob(runningJob.jobId, (job) => job.state === 'cancelled', 'Encoder cancellation')
    assert.equal(cancelled.cancelRequested, false)
    assert.equal(await exists(cancelPath), false)
    await assertNoTemporary(runningJob.jobId)
    evidence.cancellation = { observedProgress: encoding.progress, acknowledged: cancelAcknowledged, terminal: cancelled }
    check('Duplicate running start is rejected; cancellation after real progress reaps the worker and removes temporary output')

    await update('timeline_undo', {})
    await update('timeline_undo', {})
    assert.deepEqual((await call('get_state')).project.tracks, stateBefore.project.tracks)
    const finalProjectPath = join(runDirectory, 'MCP 导出验收.kaicut')
    await update('project_save_as', { path: finalProjectPath })
    report.projectPath = finalProjectPath
    evidence.finalSession = { revision: context().revision, dirty: (await call('get_state')).dirty }
    check('Temporary test edits are undone and the final project is saved separately')
  } catch (error) {
    workflowError = error
    evidence.error = error.message
  } finally {
    // A timed-out prepare may have committed before its reply was lost. Reconcile
    // only our explicit output paths, never cancel an unrelated application's job.
    try {
      for (const job of await call('export_job_list')) {
        if (job.projectId === stateBefore.project.projectId && requestedOutputs.has(job.outputPath)) ownedJobs.add(job.jobId)
      }
    } catch (error) { cleanupErrors.push({ error: `Could not reconcile prepared jobs: ${error.message}` }) }
    for (const jobId of ownedJobs) {
      try {
        const job = await jobStatus(jobId)
        if (job.state === 'queued' || job.state === 'running') {
          await call('export_job_cancel', { jobId })
          await waitForJob(jobId, (current) => terminal.has(current.state), 'Clean up owned validation job')
        }
      } catch (error) { cleanupErrors.push({ jobId, error: error.message }) }
    }
    if (cleanupErrors.length) {
      evidence.cleanupErrors = cleanupErrors
    }
  }
  if (cleanupErrors.length) throw new AggregateError(workflowError ? [workflowError] : [], 'Some owned export validation jobs could not be stopped; inspect export report before closing the test instance')
  if (workflowError) throw workflowError
  return evidence
}
