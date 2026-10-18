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
const client = new Client({ name: 'kaicut-playback-check', version: '0.1.0' })
const runFile = promisify(execFile)
const report = { ok: false, samples: [], transitions: [], seeks: [], evidence: 'Real hidden WebView media clocks; not WASAPI/device-output timing or visible presentation latency.' }
let session
let directory
let connected = false
const sources = []
const fps = 30

function options() {
  const output = {}
  const argv = process.argv.slice(2)
  for (let i = 0; i < argv.length; i += 2) {
    assert.ok(['--connection-file', '--output-dir', '--fixtures', '--seconds'].includes(argv[i]) && argv[i + 1] && !output[argv[i]], 'Expected --output-dir <target directory> [--connection-file <path>] [--fixtures <previous run>] [--seconds 30..600]')
    output[argv[i]] = argv[i + 1]
  }
  assert.ok(output['--output-dir'])
  output.seconds = Number(output['--seconds'] ?? 600)
  assert.ok(Number.isInteger(output.seconds) && output.seconds >= 30 && output.seconds <= 600)
  return output
}
function within(root, path) {
  const value = relative(root, path)
  return value === '' || (value !== '..' && !value.startsWith(`..${sep}`) && !isAbsolute(value))
}
async function fingerprint(path) {
  const info = await stat(path)
  assert.ok(info.isFile() && info.size < 512 * 1024 * 1024)
  return { path, size: info.size, mtimeMs: info.mtimeMs, sha256: createHash('sha256').update(await readFile(path)).digest('hex') }
}
function statistics(values) {
  assert.ok(values.length > 0 && values.every(Number.isFinite), 'Missing/invalid metric')
  const sorted = [...values].sort((a, b) => a - b)
  return { count: sorted.length, min: sorted[0], median: sorted[Math.floor(sorted.length / 2)], p95: sorted[Math.ceil(sorted.length * .95) - 1], max: sorted.at(-1) }
}
const context = () => ({ projectId: session.project.projectId, revision: session.revision })
async function call(name, args = {}) {
  const result = await client.callTool({ name, arguments: args }, undefined, { timeout: name === 'media_probe' ? 65000 : name.startsWith('project_') ? 35000 : 10000 })
  if (result.isError) throw new Error(`${name}: ${result.content[0]?.text || 'failed'}`)
  const value = JSON.parse(result.content[0].text)
  if (name.startsWith('ui_')) {
    assert.equal(value.nativeWindow?.visible, false)
    assert.equal(value.nativeWindow?.focused, false)
  }
  return value
}
function expected(frame) {
  const active = (track) => track.clips.find((clip) => frame >= clip.startFrame && frame < clip.startFrame + clip.durationFrames)
  const visual = session.project.tracks.filter((track) => track.kind === 'video' && !track.muted).map(active).find(Boolean)
  return [...(visual ? [{ kind: 'video', clip: visual }] : []), ...session.project.tracks.filter((track) => track.kind === 'audio' && !track.muted).map(active).filter(Boolean).map((clip) => ({ kind: 'audio', clip }))]
}
function identity(nodes) { return nodes.map((node) => `${node.kind}:${node.clipId}:${node.mediaId}`).sort() }
function actualIdentity(nodes) { return nodes.map((node) => `${node.kind}:${node.clipId}:${node.mediaId}:${node.nodeId}`).sort() }
function mapped(ui) {
  const items = expected(ui.playheadFrame)
  return ui.projectId === session.project.projectId && ui.revision === session.revision && ui.preview.mode === 'timeline'
    && JSON.stringify(identity(ui.mediaNodes)) === JSON.stringify(identity(items.map(({ kind, clip }) => ({ kind, clipId: clip.id, mediaId: clip.mediaId }))))
    && ui.mediaNodes.every((node) => {
      const item = items.find(({ clip }) => clip.id === node.clipId)
      return Number.isFinite(node.currentTime) && Number.isFinite(node.expectedSourceTime) && Number.isFinite(node.driftSeconds)
        && Math.abs(node.expectedSourceTime - (item.clip.sourceInFrame + ui.playheadFrame - item.clip.startFrame) / fps) < 1e-6
        && Math.abs(node.driftSeconds - (node.currentTime - node.expectedSourceTime)) < 1e-6
    })
}
function ready(ui) { return mapped(ui) && ui.mediaNodes.every((node) => node.readyState >= 2 && !node.seeking && node.errorCode === null) }
async function waitFor(predicate, label, timeout = 12000) {
  const deadline = performance.now() + timeout
  let ui
  while (performance.now() < deadline) {
    ui = await call('ui_inspect')
    if (predicate(ui)) return ui
    await delay(100)
  }
  throw new Error(`${label}: timed out ${JSON.stringify({ frame: ui?.playheadFrame, preview: ui?.preview, nodes: ui?.mediaNodes, alerts: ui?.alerts })}`)
}
async function update(name, args) {
  session = await call(name, { ...context(), ...args })
  await waitFor((ui) => ui.projectId === session.project.projectId && ui.revision === session.revision, 'UI revision')
}
async function seek(frame) {
  await call('ui_seek', { ...context(), frame })
  const ui = await waitFor((view) => view.playheadFrame === frame && ready(view) && view.mediaNodes.every((node) => Math.abs(node.driftSeconds) < .002), 'Explicit seek settled')
  report.seeks.push({ frame, nodes: ui.mediaNodes, preview: ui.preview })
  return ui
}
async function pauseAndCheck() {
  await call('ui_pause', context())
  const baseline = await waitFor((ui) => !ui.preview.playing && ready(ui) && ui.mediaNodes.every((node) => node.paused && node.playbackRate === 1), 'Pause settled')
  await delay(350)
  const after = await call('ui_inspect')
  assert.ok(ready(after), 'Paused nodes must retain independently verified source mappings')
  assert.equal(after.playheadFrame, baseline.playheadFrame)
  assert.deepEqual(actualIdentity(after.mediaNodes), actualIdentity(baseline.mediaNodes))
  assert.equal(after.preview.playing, false)
  for (const node of after.mediaNodes) {
    assert.equal(node.paused, true)
    assert.equal(node.playbackRate, 1)
    assert.ok(Math.abs(node.currentTime - baseline.mediaNodes.find((other) => other.clipId === node.clipId).currentTime) < .002)
  }
  return after
}
async function generate() {
  const video = join(directory, '610秒动态画面.mp4')
  const audio = join(directory, '610秒节拍.wav')
  const sampleRate = 48000
  const frames = sampleRate * 610
  const bytes = Buffer.alloc(44 + frames * 2)
  bytes.write('RIFF', 0); bytes.writeUInt32LE(bytes.length - 8, 4); bytes.write('WAVEfmt ', 8)
  bytes.writeUInt32LE(16, 16); bytes.writeUInt16LE(1, 20); bytes.writeUInt16LE(1, 22)
  bytes.writeUInt32LE(sampleRate, 24); bytes.writeUInt32LE(sampleRate * 2, 28); bytes.writeUInt16LE(2, 32); bytes.writeUInt16LE(16, 34)
  bytes.write('data', 36); bytes.writeUInt32LE(frames * 2, 40)
  for (let i = 0; i < frames; i++) bytes.writeInt16LE(Math.round((i % sampleRate < sampleRate / 10 ? .2 : .03) * Math.sin(2 * Math.PI * 440 * i / sampleRate) * 32767), 44 + i * 2)
  await writeFile(audio, bytes, { flag: 'wx' })
  await runFile(join(workspace, 'src-tauri', 'binaries', 'ffmpeg-x86_64-pc-windows-msvc.exe'), ['-hide_banner', '-v', 'error', '-nostdin', '-n', '-f', 'lavfi', '-i', 'testsrc2=size=640x360:rate=30:duration=610', '-an', '-c:v', 'h264_mf', '-b:v', '600k', '-pix_fmt', 'nv12', video], { windowsHide: true, timeout: 240000, maxBuffer: 1024 * 1024 })
  return [video, audio]
}
async function openProject(base, title, definitions) {
  const project = structuredClone(base)
  project.projectId = `project-${randomUUID()}`
  project.name = title
  for (const track of project.tracks) track.clips = []
  for (const [kind, id, start, sourceIn, duration] of definitions) {
    const track = project.tracks.find((item) => item.kind === kind)
    const asset = project.mediaRefs.find((item) => item.kind === kind)
    track.clips.push({ id, mediaId: asset.id, trackId: track.id, startFrame: start * fps, sourceInFrame: sourceIn * fps, durationFrames: duration * fps, sourceDurationFrames: 610 * fps, fadeInFrames: 0, fadeOutFrames: 0 })
  }
  const path = join(directory, `${title}.kaicut`)
  await writeFile(path, JSON.stringify(project, null, 2), { flag: 'wx' })
  await update('project_open_path', { path })
  await waitFor(ready, 'Opened decoders ready')
  return path
}
function sample(ui, elapsedMs) { return { elapsedMs, frame: ui.playheadFrame, preview: ui.preview, nodes: ui.mediaNodes, alerts: ui.alerts, metrics: ui.performance, nativeWindow: ui.nativeWindow, viewport: ui.viewport } }

function checkClock(ui) {
  const active = expected(ui.playheadFrame)
  const master = active.find((item) => item.kind === 'audio') ?? active.find((item) => item.kind === 'video')
  assert.equal(ui.preview.clockMode, master?.kind ?? 'monotonic', 'Reported clock must follow the actual active project streams')
  assert.equal(ui.preview.masterKind, master?.kind ?? null)
  assert.equal(ui.preview.masterClipId, master?.clip.id ?? null)
  if (ui.preview.playing) assert.equal(typeof ui.preview.buffering, 'boolean', 'Playing clock must report its buffering state')
  if (ready(ui) && !ui.preview.buffering) {
    for (const node of ui.mediaNodes) {
      assert.ok(typeof node.nodeId === 'string' && node.nodeId.length > 0, 'Every active media element needs a measurable identity')
      assert.ok(Number.isFinite(node.playbackRate) && node.playbackRate >= .97 && node.playbackRate <= 1.03, 'Slave rate correction exceeded its declared range')
      assert.ok(Number.isSafeInteger(node.hardSeekCount) && node.hardSeekCount >= 0, 'Hard-seek counter is unavailable')
      if (node.clipId === master?.clip.id) assert.equal(node.playbackRate, 1, 'Master media must not be rate-corrected')
    }
  }
}

function diagnostics(samples) {
  let bufferingMs = 0, currentBufferMs = 0, maxBufferingMs = 0, frameStillMs = 0, maxFrameStillMs = 0
  const modes = {}
  let bufferingSamples = 0, unavailableBufferingSamples = 0, unreadySamples = 0
  for (let index = 0; index < samples.length; index++) {
    const item = samples[index]
    const elapsed = index ? Math.max(0, item.elapsedMs - samples[index - 1].elapsedMs) : 0
    modes[item.preview.clockMode ?? 'unavailable'] = (modes[item.preview.clockMode ?? 'unavailable'] ?? 0) + 1
    if (item.preview.buffering === true) {
      bufferingSamples++; bufferingMs += elapsed; currentBufferMs += elapsed; maxBufferingMs = Math.max(maxBufferingMs, currentBufferMs)
    } else { if (item.preview.buffering !== false) unavailableBufferingSamples++; currentBufferMs = 0 }
    if (index && item.frame === samples[index - 1].frame && item.preview.playing) { frameStillMs += elapsed; maxFrameStillMs = Math.max(maxFrameStillMs, frameStillMs) } else frameStillMs = 0
    if (item.nodes.some((node) => node.readyState < 2 || node.seeking || node.paused || !Number.isFinite(node.driftSeconds))) unreadySamples++
  }
  return { sampleCount: samples.length, bufferingSamples, unavailableBufferingSamples, unreadySamples,
    estimatedBufferingMs: bufferingMs, estimatedLongestBufferingMs: maxBufferingMs, estimatedLongestUnchangedFrameMs: maxFrameStillMs, clockModeSamples: modes,
    evidence: 'All observed samples including startup and boundaries; durations are polling estimates, not exact event timestamps.' }
}

function boundarySummary(samples) {
  return [60, 120, 180, 240, 300, 360, 420].map((boundary) => {
    const nearby = samples.filter((item) => item.frame >= boundary - 15 && item.frame <= boundary + 15)
    const drifts = nearby.flatMap((item) => item.nodes.map((node) => node.driftSeconds)).filter(Number.isFinite).map(Math.abs)
    const crossing = samples.find((item) => item.frame >= boundary)
    const recovered = crossing && samples.find((item) => item.elapsedMs >= crossing.elapsedMs && item.frame >= boundary
      && item.preview.buffering === false && item.nodes.every((node) => node.readyState >= 2 && !node.seeking && (!item.preview.playing || !node.paused)))
    return { boundaryFrame: boundary, observedSamples: nearby.length, firstObservedAtMs: crossing?.elapsedMs ?? null,
      recoveryAfterObservedCrossingMs: recovered ? recovered.elapsedMs - crossing.elapsedMs : null,
      maxFiniteMediaDriftSeconds: drifts.length ? Math.max(...drifts) : null, ...diagnostics(nearby) }
  })
}

try {
  const args = options()
  report.requestedSeconds = args.seconds
  report.fullTenMinuteRun = args.seconds === 600
  const output = resolve(args['--output-dir'])
  assert.ok(within(target, output))
  await mkdir(output, { recursive: true })
  assert.ok(within(await realpath(target), await realpath(output)))
  directory = join(output, `run-${randomUUID()}`)
  await mkdir(directory)
  report.directory = directory
  await client.connect(new StdioClientTransport({ command: process.execPath,
    args: [join(workspace, 'mcp', 'server.mjs'), '--connection-file', connectionFilePath(args['--connection-file'] ? ['--connection-file', args['--connection-file']] : [])], stderr: 'inherit', maxBufferSize: 17 * 1024 * 1024 }))
  connected = true
  session = await call('get_state')
  assert.ok(!session.dirty && !session.path && !session.canUndo && !session.project.mediaRefs.length && session.project.tracks.every((track) => !track.clips.length), 'Use a new isolated empty validation instance')
  report.initialWindow = (await call('ui_inspect')).nativeWindow
  console.log('Preparing 610-second generated media for actual wall-time playback.')
  const paths = args['--fixtures'] ? ['610秒动态画面.mp4', '610秒节拍.wav'].map((name) => join(resolve(args['--fixtures']), name)) : await generate()
  sources.push(...await Promise.all(paths.map(fingerprint)))
  await update('project_import_paths', { paths })
  const probes = await call('media_probe')
  assert.ok(probes.length === 2 && probes.every((probe) => probe.state === 'ready' && Math.abs(probe.durationMs - 610000) < 100))
  await update('project_save_as', { path: join(directory, '测试素材.kaicut') })
  const base = session.project
  report.continuousProject = await openProject(base, '音频主时钟连续验证', [['video', 'video-full', 0, 0, 610], ['audio', 'audio-full', 0, 0, 610]])
  await seek(0)
  await call('ui_play', context())
  const started = performance.now()
  let progressAt = started
  let lastChanged = started
  let previousFrame = 0
  // Allow a bounded startup/buffering margin; never speed up the test clock.
  const deadline = started + (args.seconds + 20) * 1000
  while (true) {
    const ui = await call('ui_inspect')
    const now = performance.now()
    report.samples.push(sample(ui, now - started))
    checkClock(ui)
    assert.equal(ui.preview.playing, true)
    assert.equal(ui.alerts.length, 0)
    assert.ok(ui.playheadFrame >= previousFrame)
    if (ui.playheadFrame > previousFrame) lastChanged = now
    assert.ok(now - lastChanged < 2000, 'Audio master stalled for more than two seconds')
    assert.ok(now < deadline, 'Playback did not cover the requested project duration within its wall-time budget')
    assert.ok(ui.mediaNodes.every((node) => node.errorCode === null))
    if (now - started >= 5000 && ui.preview.buffering === false) {
      assert.ok(ready(ui), 'Nonbuffering continuous playback must have correct decoded audio/video mappings')
      assert.ok(ui.mediaNodes.every((node) => !node.paused), 'Nonbuffering playback left a media stream paused')
    }
    if (now - progressAt >= 30000) {
      console.log(JSON.stringify({ wallSeconds: (now - started) / 1000, timelineSeconds: ui.playheadFrame / fps, preview: ui.preview, drift: ui.mediaNodes.map(({ kind, driftSeconds, playbackRate, hardSeekCount }) => ({ kind, driftSeconds, playbackRate, hardSeekCount })) }))
      progressAt = now
    }
    previousFrame = ui.playheadFrame
    if (ui.playheadFrame >= args.seconds * fps && now - started >= args.seconds * 1000) break
    await delay(450)
  }
  report.paused = sample(await pauseAndCheck(), performance.now() - started)
  const eligible = report.samples.filter((item) => item.elapsedMs >= 5000)
  const settled = eligible.filter((item) => item.preview.buffering === false && item.preview.clockMode === 'audio' && item.nodes.length === 2 && item.nodes.every((node) => node.readyState >= 2 && !node.seeking && !node.paused && Number.isFinite(node.driftSeconds)))
  assert.ok(settled.length >= eligible.length * .9, 'Too many unready/buffering samples after startup; do not hide them in P95')
  const av = settled.map((item) => Math.abs(item.nodes.find((node) => node.kind === 'audio').driftSeconds - item.nodes.find((node) => node.kind === 'video').driftSeconds))
  const first = report.samples[0], last = report.samples.at(-1)
  report.summary = { sampleCount: report.samples.length, eligibleCount: eligible.length, settledCount: settled.length, relativeClockSeconds: statistics(av), allSamples: diagnostics(report.samples),
    mediaTimelineSeconds: statistics(settled.flatMap((item) => item.nodes.map((node) => Math.abs(node.driftSeconds)))),
    playbackRate: ((last.frame - first.frame) / fps) / ((last.elapsedMs - first.elapsedMs) / 1000),
    wallSeconds: last.elapsedMs / 1000, timelineSeconds: last.frame / fps,
    hardSeekCounts: last.nodes.map(({ kind, hardSeekCount }) => ({ kind, hardSeekCount })),
    oneFrameSeconds: 1 / fps, oneFrameP95: statistics(av).p95 < 1 / fps, oneFrameAllSettled: statistics(av).max < 1 / fps }
  assert.ok(report.summary.playbackRate > .98 && report.summary.playbackRate < 1.02)
  for (const frame of [15, 9000, 18270, 0]) await seek(frame)

  report.transitionsProject = await openProject(base, '连续切段与空隙验证', [
    ['video', 'video-a', 0, 10, 2], ['video', 'video-b', 2, 12, 2],
    ['audio', 'audio-a', 0, 10, 2], ['audio', 'audio-b', 2, 12, 2],
    ['video', 'video-c', 4, 40, 2], ['audio', 'audio-c', 4, 40, 2],
    ['video', 'video-jump', 8, 50, 2], ['audio', 'audio-jump', 8, 50, 2],
    ['audio', 'audio-tail', 10, 52, 2], ['video', 'video-tail', 12, 80, 2],
  ])
  await call('ui_play', context())
  const transitionsStarted = performance.now()
  while (performance.now() - transitionsStarted < 20000) {
    const ui = await call('ui_inspect')
    report.transitions.push(sample(ui, performance.now() - transitionsStarted))
    checkClock(ui)
    assert.equal(ui.alerts.length, 0)
    if (!ui.preview.playing) { assert.equal(ui.playheadFrame, 420, 'Transition playback stopped before timeline end'); break }
    const previous = report.transitions.at(-2)
    if (previous) assert.ok(ui.playheadFrame >= previous.frame, 'Transition playback moved backwards')
    assert.ok(ui.mediaNodes.every((node) => node.errorCode === null))
    await delay(150)
  }
  assert.equal(report.transitions.at(-1).frame, 420)
  assert.equal(report.transitions.at(-1).preview.playing, false)
  for (const kind of ['video', 'audio']) {
    const before = report.transitions.find((item) => item.frame >= 30 && item.frame < 50)?.nodes.find((node) => node.kind === kind)
    const after = report.transitions.find((item) => item.frame >= 75 && item.frame < 105)?.nodes.find((node) => node.kind === kind)
    assert.ok(before && after && typeof before.nodeId === 'string')
    assert.ok(Number.isSafeInteger(before.hardSeekCount) && Number.isSafeInteger(after.hardSeekCount))
    assert.equal(after.nodeId, before.nodeId, 'Continuous cuts must reuse the same actual media element')
    assert.equal(after.hardSeekCount, before.hardSeekCount, 'Continuous cuts must not issue a new hard seek')
    const jumpedSample = report.transitions.find((item) => item.frame >= 135 && item.frame < 165
      && item.preview.buffering === false && item.nodes.some((node) => node.kind === kind && node.clipId === `${kind}-c` && node.readyState >= 2 && !node.seeking))
    const jumped = jumpedSample?.nodes.find((node) => node.kind === kind)
    assert.ok(jumped && jumpedSample, 'Adjacent noncontinuous source cut must become ready')
    assert.equal(jumped.nodeId, after.nodeId, 'Adjacent same-source discontinuity must reuse the actual media element')
    assert.ok(Number.isSafeInteger(jumped.hardSeekCount) && jumped.hardSeekCount >= after.hardSeekCount + 1, 'Adjacent source discontinuity must issue at least one new hard seek')
    assert.ok(Math.abs(jumped.currentTime - (40 + (jumpedSample.frame - 120) / fps)) < .15, 'Adjacent source discontinuity must play from its new source mapping')
  }
  assert.ok(report.transitions.some((item) => item.frame >= 195 && item.frame < 225 && item.nodes.length === 0), 'Silent gap must advance without media')
  assert.ok(report.transitions.some((item) => item.frame >= 255 && item.frame < 285 && item.nodes.some((node) => node.clipId === 'video-jump' && Math.abs(node.currentTime - (50 + (item.frame - 240) / fps)) < .15)), 'Discontinuous source jump must use its source time')
  assert.ok(report.transitions.some((item) => item.frame >= 315 && item.frame < 345 && item.nodes.length === 1 && item.nodes[0].kind === 'audio'))
  assert.ok(report.transitions.some((item) => item.frame >= 375 && item.frame < 405 && item.nodes.length === 1 && item.nodes[0].kind === 'video'))
  for (const item of report.transitions.filter((entry) => entry.preview.playing && entry.preview.buffering === false && entry.frame % 60 >= 15)) {
    assert.ok(ready({ ...item, mediaNodes: item.nodes, playheadFrame: item.frame, projectId: session.project.projectId, revision: session.revision }), 'Settled transition must use correct media and source mappings')
    assert.ok(item.nodes.every((node) => !node.paused))
  }
  await pauseAndCheck()
  // A failed quality gate remains a failed report even when lifecycle checks passed.
  assert.ok(report.summary.oneFrameP95, 'Relative A/V media-clock P95 remains above one project frame')
  assert.ok(report.summary.oneFrameAllSettled, 'Some settled A/V samples exceed one project frame; full precision target not achieved')
  assert.equal((await call('get_state')).dirty, false)
  report.ok = true
} catch (error) {
  report.error = error.message
  process.exitCode = 1
} finally {
  report.allSampleDiagnostics = diagnostics(report.samples)
  report.transitionDiagnostics = diagnostics(report.transitions)
  report.boundaries = boundarySummary(report.transitions)
  if (connected && session) {
    try { await call('ui_pause', context()) } catch (error) { report.cleanupError = error.message; report.ok = false; process.exitCode = 1 }
  }
  try {
    for (const source of sources) assert.deepEqual(await fingerprint(source.path), source)
    report.sources = sources
  } catch (error) { report.integrityError = error.message; report.ok = false; process.exitCode = 1 }
  await client.close()
  if (directory) await writeFile(join(directory, 'playback-validation.json'), JSON.stringify(report, null, 2), { flag: 'wx' })
  console.log(JSON.stringify({ ok: report.ok, error: report.error, directory, summary: report.summary, transitionSamples: report.transitions.length, seekCount: report.seeks.length, fullTenMinuteRun: report.fullTenMinuteRun }))
}
