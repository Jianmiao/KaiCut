#!/usr/bin/env node
import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { createHash, randomUUID } from 'node:crypto'
import { mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path'
import { performance } from 'node:perf_hooks'
import { setTimeout as delay } from 'node:timers/promises'
import { fileURLToPath } from 'node:url'
import { promisify } from 'node:util'
import { Client } from '@modelcontextprotocol/sdk/client/index.js'
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js'
import { connectionFilePath } from './bridge.mjs'

const workspace = dirname(dirname(fileURLToPath(import.meta.url)))
const target = join(workspace, 'src-tauri', 'target')
const runFile = promisify(execFile)
const client = new Client({ name: 'kaicut-preview-check', version: '0.1.0' })
const report = { ok: false, profiles: [], seeks: [], evidence: 'Hidden real Tauri/WebView2 media-clock sampling, not physical audio latency or visible-window frame pacing.' }
let directory
let session
let connected = false
const fingerprints = []

function options() {
  const result = {}
  const args = process.argv.slice(2)
  for (let i = 0; i < args.length; i += 2) {
    if (!['--connection-file', '--output-dir', '--fixtures'].includes(args[i]) || !args[i + 1] || result[args[i]]) throw new Error('Usage: preview-check --output-dir <src-tauri/target directory> [--connection-file <path>] [--fixtures <previous preview-check run directory>]')
    result[args[i]] = args[i + 1]
  }
  assert.ok(result['--output-dir'], '--output-dir is required')
  return result
}
function within(base, path) {
  const part = relative(base, path)
  return part === '' || (part !== '..' && !part.startsWith(`..${sep}`) && !isAbsolute(part))
}
function stats(values) {
  assert.ok(values.every(Number.isFinite), 'Metric includes unavailable or non-finite samples')
  const sorted = [...values].sort((a, b) => a - b)
  assert.ok(sorted.length, 'Metric has no samples')
  return { count: sorted.length, min: sorted[0], median: sorted[Math.floor(sorted.length / 2)], p95: sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * 0.95) - 1)], max: sorted.at(-1) }
}
async function fingerprint(path) {
  const info = await stat(path)
  assert.ok(info.isFile() && info.size < 100 * 1024 * 1024)
  return { path, size: info.size, mtimeMs: info.mtimeMs, sha256: createHash('sha256').update(await readFile(path)).digest('hex') }
}
function compactProfile({ samples: _samples, ...profile }) {
  if (!profile.summary?.videoQuality) return profile
  const { sameClipDeltas, ...quality } = profile.summary.videoQuality
  return { ...profile, summary: { ...profile.summary, videoQuality: { ...quality,
    sampledFrameDelta: sameClipDeltas.reduce((sum, item) => sum + item.totalDelta, 0),
    sampledDroppedDelta: sameClipDeltas.every((item) => item.droppedDelta !== null) ? sameClipDeltas.reduce((sum, item) => sum + item.droppedDelta, 0) : null } } }
}
async function call(name, args = {}) {
  const start = performance.now()
  const result = await client.callTool({ name, arguments: args }, undefined, { timeout: ['media_probe'].includes(name) ? 65000 : name.startsWith('project_') ? 35000 : 10000 })
  if (result.isError) throw new Error(`${name}: ${result.content[0]?.text || 'failed'}`)
  const value = JSON.parse(result.content[0].text)
  if (name.startsWith('ui_')) {
    assert.equal(value.nativeWindow?.visible, false, 'Validation requires a hidden native window')
    assert.equal(value.nativeWindow?.focused, false, 'Validation must not take keyboard focus')
    value.roundTripMs = performance.now() - start
  }
  return value
}
const context = () => ({ projectId: session.project.projectId, revision: session.revision })
async function waitFor(predicate, description, timeout = 15000) {
  const deadline = performance.now() + timeout
  let ui
  while (performance.now() < deadline) {
    ui = await call('ui_inspect')
    if (predicate(ui)) return ui
    await delay(100)
  }
  throw new Error(`${description}: timed out; frame=${ui?.playheadFrame}; nodes=${JSON.stringify(ui?.mediaNodes)}; alerts=${JSON.stringify(ui?.alerts)}`)
}
async function update(name, args) {
  session = await call(name, { ...context(), ...args })
  await waitFor((ui) => ui.projectId === session.project.projectId && ui.revision === session.revision, 'UI revision')
}
function expectedMedia(frame) {
  const active = (track) => track.clips.find((clip) => frame >= clip.startFrame && frame < clip.startFrame + clip.durationFrames)
  const visible = session.project.tracks.filter((track) => track.kind === 'video' && !track.muted).map(active).find(Boolean)
  const audio = session.project.tracks.filter((track) => track.kind === 'audio' && !track.muted).map(active).filter(Boolean)
  const { fpsNumerator, fpsDenominator } = session.project.timebase
  return [...(visible ? [{ kind: 'video', clip: visible }] : []), ...audio.map((clip) => ({ kind: 'audio', clip }))]
    .map(({ kind, clip }) => ({ kind, clipId: clip.id, mediaId: clip.mediaId,
      sourceTime: (clip.sourceInFrame + frame - clip.startFrame) * fpsDenominator / fpsNumerator }))
}
const nodeKeys = (nodes) => nodes.map((node) => `${node.kind}:${node.clipId}:${node.mediaId}`).sort()
function matchesExpectedMedia(ui) {
  const expected = expectedMedia(ui.playheadFrame)
  return ui.projectId === session.project.projectId && ui.revision === session.revision && ui.preview.mode === 'timeline'
    && JSON.stringify(nodeKeys(ui.mediaNodes)) === JSON.stringify(nodeKeys(expected))
}
function validClocks(ui) {
  const expected = expectedMedia(ui.playheadFrame)
  return matchesExpectedMedia(ui) && ui.mediaNodes.every((node) => {
    const item = expected.find((entry) => entry.kind === node.kind && entry.clipId === node.clipId)
    return Number.isFinite(node.currentTime) && Number.isFinite(node.expectedSourceTime) && Number.isFinite(node.driftSeconds)
      && Math.abs(node.expectedSourceTime - item.sourceTime) < 0.000001
      && Math.abs(node.driftSeconds - (node.currentTime - item.sourceTime)) < 0.000001
  })
}
function ready(ui) {
  return ui.mediaNodes.length === 2 && validClocks(ui)
    && ui.mediaNodes.every((node) => node.readyState >= 2 && !node.seeking && node.errorCode === null && Math.abs(node.driftSeconds) < 0.05)
}

async function generateFixtures() {
  const video = join(directory, '90秒动态画面.mp4')
  const audio = join(directory, '90秒节拍.wav')
  const rate = 48000
  const frames = rate * 90
  const wav = Buffer.alloc(44 + frames * 2)
  wav.write('RIFF', 0); wav.writeUInt32LE(wav.length - 8, 4); wav.write('WAVEfmt ', 8)
  wav.writeUInt32LE(16, 16); wav.writeUInt16LE(1, 20); wav.writeUInt16LE(1, 22)
  wav.writeUInt32LE(rate, 24); wav.writeUInt32LE(rate * 2, 28); wav.writeUInt16LE(2, 32); wav.writeUInt16LE(16, 34)
  wav.write('data', 36); wav.writeUInt32LE(frames * 2, 40)
  for (let i = 0; i < frames; i++) {
    const amplitude = i % rate < rate / 10 ? 0.25 : 0.05
    wav.writeInt16LE(Math.round(amplitude * Math.sin(2 * Math.PI * 440 * i / rate) * 32767), 44 + i * 2)
  }
  await writeFile(audio, wav, { flag: 'wx' })
  const binary = join(workspace, 'src-tauri', 'binaries', 'ffmpeg-x86_64-pc-windows-msvc.exe')
  await runFile(binary, ['-v', 'error', '-nostdin', '-n', '-f', 'lavfi', '-i', 'testsrc2=size=1280x720:rate=30:duration=90', '-an', '-c:v', 'h264_mf', '-b:v', '2M', '-pix_fmt', 'nv12', video], { windowsHide: true, timeout: 180000, maxBuffer: 1024 * 1024 })
  return [video, audio]
}

async function openProfile(base, name, count, durationFrames) {
  const project = structuredClone(base)
  project.projectId = `project-${randomUUID()}`
  project.name = name
  const video = project.mediaRefs.find((media) => media.kind === 'video')
  const audio = project.mediaRefs.find((media) => media.kind === 'audio')
  for (const track of project.tracks) track.clips = []
  for (const [kind, media] of [['video', video], ['audio', audio]]) {
    const track = project.tracks.find((item) => item.kind === kind)
    for (let i = 0; i < count; i++) {
      track.clips.push({ id: `${kind}-${i}`, mediaId: media.id, trackId: track.id,
        startFrame: i * durationFrames, durationFrames, sourceInFrame: count === 1 ? 0 : i % 14 * 180,
        sourceDurationFrames: 2700, fadeInFrames: 0, fadeOutFrames: 0 })
    }
  }
  const path = join(directory, `${name}.kaicut`)
  await writeFile(path, `${JSON.stringify(project, null, 2)}\n`, { flag: 'wx' })
  const start = performance.now()
  await update('project_open_path', { path })
  await waitFor(ready, 'First decoded frame after project open')
  return { name, path, clipCount: count * 2, durationSeconds: count * durationFrames / 30, openReadyMs: performance.now() - start }
}

async function seek(frame) {
  const start = performance.now()
  await call('ui_seek', { ...context(), frame })
  const ui = await waitFor((view) => view.playheadFrame === frame && ready(view), `Seek to ${frame}`)
  report.seeks.push({ project: session.project.name, frame, settledMs: performance.now() - start, mediaNodes: ui.mediaNodes, domNodes: ui.performance.domNodeCount, visibleClips: ui.performance.timelineClipElementCount })
  return ui
}

async function samplePlayback(profile, seconds, segmentFrames) {
  const samples = []
  profile.samples = samples
  const before = await waitFor(ready, 'Playback readiness')
  const started = performance.now()
  let lastAdvancedAt = started
  let previousFrame = before.playheadFrame
  try {
    await call('ui_play', context())
    while (performance.now() - started < seconds * 1000) {
      const ui = await call('ui_inspect')
      const elapsed = performance.now() - started
      samples.push({ elapsedMs: elapsed, frame: ui.playheadFrame, playing: ui.preview.playing, alerts: ui.alerts, mediaNodes: ui.mediaNodes, metrics: ui.performance, roundTripMs: ui.roundTripMs })
      assert.equal(ui.preview.playing, true, 'Playback stopped before the requested sample interval')
      assert.ok(ui.playheadFrame >= previousFrame, 'Playback moved backwards')
      if (ui.playheadFrame > previousFrame) lastAdvancedAt = performance.now()
      assert.ok(performance.now() - lastAdvancedAt < 1500, 'Playhead stalled for over 1.5 seconds')
      assert.equal(ui.alerts.length, 0, 'Preview reported an alert')
      assert.ok(ui.mediaNodes.length <= 3, 'Inactive clips must not create background decoders')
      assert.ok(ui.mediaNodes.every((node) => node.errorCode === null), 'Live media decoder failed')
      // Allow decoder handoff immediately after a boundary, then require the
      // independently resolved project clips rather than trusting node labels.
      if (ui.playheadFrame % segmentFrames >= 15 && ui.mediaNodes.length === 2
        && ui.mediaNodes.every((node) => node.readyState >= 2 && !node.seeking && !node.paused)) {
        assert.ok(validClocks(ui), 'Settled playback mounted the wrong project clips or returned unavailable/inconsistent clocks')
      }
      previousFrame = ui.playheadFrame
      await delay(250)
    }
  } finally { await call('ui_pause', context()) }
  const paused = await waitFor((ui) => ready(ui) && ui.mediaNodes.every((node) => node.paused), 'Paused media settles')
  await delay(350)
  const again = await call('ui_inspect')
  assert.equal(again.playheadFrame, paused.playheadFrame)
  assert.equal(again.preview.playing, false)
  assert.ok(ready(again), 'Paused decoder state and project clip identities must remain ready')
  assert.deepEqual(nodeKeys(again.mediaNodes), nodeKeys(paused.mediaNodes), 'Paused media nodes must not disappear or switch identity')
  for (const node of again.mediaNodes) {
    assert.equal(node.paused, true)
    assert.ok(Math.abs(node.currentTime - paused.mediaNodes.find((old) => old.clipId === node.clipId).currentTime) < 1 / 30)
  }
  const stable = samples.filter((sample) => sample.elapsedMs > 2000 && sample.frame % segmentFrames >= 15
    && sample.mediaNodes.length === 2 && sample.mediaNodes.every((node) => node.readyState >= 2 && !node.seeking && !node.paused
      && Number.isFinite(node.currentTime) && Number.isFinite(node.expectedSourceTime) && Number.isFinite(node.driftSeconds)))
  assert.ok(stable.length >= samples.length * 0.65, 'Too few settled playback samples; inspect buffering and segment transitions')
  const drift = stable.flatMap((sample) => sample.mediaNodes.map((node) => Math.abs(node.driftSeconds)))
  const av = stable.map((sample) => Math.abs(sample.mediaNodes.find((node) => node.kind === 'video').driftSeconds - sample.mediaNodes.find((node) => node.kind === 'audio').driftSeconds))
  const qualityPrevious = new Map()
  const qualityDeltas = []
  let qualitySamples = 0
  let qualityResets = 0
  for (const sample of samples) {
    for (const node of sample.mediaNodes.filter((item) => item.kind === 'video' && Number.isFinite(item.totalVideoFrames))) {
      qualitySamples += 1
      const previous = qualityPrevious.get(node.clipId)
      if (previous) {
        const totalDelta = node.totalVideoFrames - previous.totalVideoFrames
        const droppedDelta = Number.isFinite(node.droppedVideoFrames) && Number.isFinite(previous.droppedVideoFrames) ? node.droppedVideoFrames - previous.droppedVideoFrames : null
        if (totalDelta >= 0 && (droppedDelta === null || droppedDelta >= 0)) qualityDeltas.push({ clipId: node.clipId, totalDelta, droppedDelta })
        else qualityResets += 1
      }
      qualityPrevious.set(node.clipId, node)
    }
  }
  const first = samples[0]
  const last = samples.at(-1)
  const sampledSeconds = (last.elapsedMs - first.elapsedMs) / 1000
  const advancedSeconds = (last.frame - first.frame) * session.project.timebase.fpsDenominator / session.project.timebase.fpsNumerator
  const playbackRate = advancedSeconds / sampledSeconds
  assert.ok(Number.isFinite(playbackRate) && sampledSeconds > seconds - 2, 'Playback sampling did not cover the requested interval')
  assert.ok(playbackRate >= 0.95 && playbackRate <= 1.05, 'Project frames did not advance at approximately real-time speed')
  profile.summary = { sampleCount: samples.length, stableSamples: stable.length,
    absoluteMediaDriftSeconds: stats(drift), relativeAudioVideoClockSeconds: stats(av),
    inspectRoundTripMs: stats(samples.map((sample) => sample.roundTripMs)),
    inspectWorkMs: stats(samples.map((sample) => sample.metrics.inspectionDurationMs)),
    domNodes: stats(samples.map((sample) => sample.metrics.domNodeCount)), visibleTimelineClips: stats(samples.map((sample) => sample.metrics.timelineClipElementCount)),
    visitedVideoClips: [...new Set(samples.flatMap((sample) => sample.mediaNodes.filter((node) => node.kind === 'video').map((node) => node.clipId)))],
    sampledSeconds, advancedSeconds, playbackRate,
    videoQuality: { available: qualitySamples > 0, availableSamples: qualitySamples, resets: qualityResets, sameClipDeltas: qualityDeltas,
      evidence: 'Optional cumulative WebView decode counters, grouped by clip identity; not visible presentation FPS or frame pacing.' },
    finalFrame: paused.playheadFrame }
  // Declared engineering gates for this synthetic workload, not broadcast A/V-sync promises.
  assert.ok(profile.summary.absoluteMediaDriftSeconds.p95 <= 0.15, 'P95 media-clock drift exceeded 150 ms')
  assert.ok(profile.summary.relativeAudioVideoClockSeconds.p95 <= 0.15, 'P95 relative A/V media-clock drift exceeded 150 ms')
  assert.ok(profile.summary.inspectRoundTripMs.p95 <= 500, 'P95 MCP inspection latency exceeded 500 ms')
  console.log(JSON.stringify(compactProfile(profile)))
}

try {
  const args = options()
  const output = resolve(args['--output-dir'])
  assert.ok(within(target, output), 'Output must be under src-tauri/target')
  await mkdir(output, { recursive: true })
  assert.ok(within(await realpath(target), await realpath(output)), 'Output must not escape through a directory link')
  directory = join(output, `run-${randomUUID()}`)
  await mkdir(directory)
  report.directory = directory
  await client.connect(new StdioClientTransport({ command: process.execPath,
    args: [join(workspace, 'mcp', 'server.mjs'), '--connection-file', connectionFilePath(args['--connection-file'] ? ['--connection-file', args['--connection-file']] : [])], stderr: 'inherit', maxBufferSize: 17 * 1024 * 1024 }))
  connected = true
  session = await call('get_state')
  assert.ok(!session.dirty && !session.path && !session.canUndo && session.project.mediaRefs.length === 0 && session.project.tracks.every((track) => !track.clips.length), 'Use a fresh disposable --mcp-validation instance')
  report.initialWindow = (await call('ui_inspect')).nativeWindow
  console.log('Preparing 90-second synthetic video/audio; personal media is not used.')
  const paths = args['--fixtures'] ? ['90秒动态画面.mp4', '90秒节拍.wav'].map((name) => join(resolve(args['--fixtures']), name)) : await generateFixtures()
  fingerprints.push(...await Promise.all(paths.map(fingerprint)))
  await update('project_import_paths', { paths })
  const probes = await call('media_probe')
  assert.ok(probes.length === 2 && probes.every((probe) => probe.state === 'ready' && Math.abs(probe.durationMs - 90000) < 100))
  await update('project_save_as', { path: join(directory, '测试素材.kaicut') })
  const base = session.project
  const continuous = await openProfile(base, '90秒连续预览', 1, 2700)
  report.profiles.push(continuous)
  console.log('Sampling 65 seconds of continuous playback.')
  await samplePlayback(continuous, 65, 2700)
  const multi = await openProfile(base, '30分钟600片段', 300, 180)
  report.profiles.push(multi)
  for (const frame of [15, 27015, 53985, 15]) await seek(frame)
  console.log('Sampling 24 seconds across adjacent clips in a 30-minute timeline.')
  await samplePlayback(multi, 24, 180)
  assert.ok(multi.summary.visitedVideoClips.length >= 4, 'Multi-clip playback must actually cross clip boundaries')
  const density = await openProfile(base, '30分钟6000片段', 3000, 18)
  report.profiles.push(density)
  for (const frame of [9, 27009, 53991]) await seek(frame)
  density.summary = { seekOnly: true, maxMountedClips: Math.max(...report.seeks.filter((sample) => sample.project === density.name).map((sample) => sample.visibleClips)) }
  assert.ok(report.seeks.filter((sample) => sample.project === density.name).every((sample) => sample.visibleClips > 0), 'Each dense seek must retain mounted timeline clips')
  assert.ok(density.summary.maxMountedClips > 0 && density.summary.maxMountedClips < 500, 'Timeline must mount some visible clips while culling offscreen clips')
  const selectionStart = performance.now()
  const selectedIds = session.project.tracks.flatMap((track) => track.clips).slice(-1000).map((clip) => clip.id)
  const selected = await call('ui_select', { ...context(), clipIds: selectedIds })
  assert.deepEqual([...selected.selectedClipIds].sort(), [...selectedIds].sort(), 'Selection must match the requested clip IDs, not only its size')
  density.summary.selection = { count: selected.selectedClipIds.length, roundTripMs: performance.now() - selectionStart,
    domNodes: selected.performance.domNodeCount, mountedClips: selected.performance.timelineClipElementCount }
  const cleared = await call('ui_select', { ...context(), clipIds: [] })
  assert.deepEqual(cleared.selectedClipIds, [], 'Clearing selection must be acknowledged')
  assert.equal(density.summary.selection.count, 1000)
  assert.ok(density.summary.selection.mountedClips > 0 && density.summary.selection.mountedClips < 500, 'Selecting offscreen clips must retain visible clips without bypassing viewport culling')
  assert.equal((await call('get_state')).dirty, false)
  report.ok = true
} catch (error) {
  report.error = error.message
  process.exitCode = 1
} finally {
  if (connected && session) {
    try { await call('ui_pause', context()) } catch (error) { report.cleanupError = error.message; report.ok = false; process.exitCode = 1 }
  }
  try {
    for (const previous of fingerprints) assert.deepEqual(await fingerprint(previous.path), previous, 'Generated source was modified')
    report.sources = fingerprints
  } catch (error) { report.ok = false; report.integrityError = error.message; process.exitCode = 1 }
  await client.close()
  if (directory) await writeFile(join(directory, 'preview-validation.json'), `${JSON.stringify(report, null, 2)}\n`, { flag: 'wx' })
  console.log(JSON.stringify({ ok: report.ok, error: report.error, directory, profiles: report.profiles.map(compactProfile), seekCount: report.seeks.length }))
}
