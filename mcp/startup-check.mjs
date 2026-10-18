#!/usr/bin/env node
import assert from 'node:assert/strict'
import { createHash, randomUUID } from 'node:crypto'
import { mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path'
import { performance } from 'node:perf_hooks'
import { setTimeout as delay } from 'node:timers/promises'
import { fileURLToPath } from 'node:url'
import { Client } from '@modelcontextprotocol/sdk/client/index.js'
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js'
import { connectionFilePath } from './bridge.mjs'

const workspace = dirname(dirname(fileURLToPath(import.meta.url)))
const target = join(workspace, 'src-tauri', 'target')
const defaultFixtures = join(target, 'mcp-playback-validation', 'run-426f5185-9799-4791-8e79-b426aed44bb3')
const client = new Client({ name: 'kaicut-startup-check', version: '0.1.0' })
const report = { ok: false, cases: [], trace: [], evidence: 'Real hidden WebView startup and transition observations; media clocks are not physical audio-output timing or visible presentation FPS.' }
const sources = []
let directory, session, connected = false
const fps = 30
const definitions = [
  ['video', 'video-a', 0, 10, 2], ['audio', 'audio-a', 0, 10, 2],
  ['video', 'video-b', 2, 12, 2], ['audio', 'audio-b', 2, 12, 2],
  ['video', 'video-c', 4, 40, 2], ['audio', 'audio-c', 4, 40, 2],
  ['video', 'video-jump', 8, 50, 2], ['audio', 'audio-jump', 8, 50, 2],
  ['audio', 'audio-tail', 10, 52, 2], ['video', 'video-tail', 12, 80, 2],
]

function options() {
  const result = {}, argv = process.argv.slice(2)
  for (let i = 0; i < argv.length; i += 2) {
    assert.ok(['--connection-file', '--output-dir', '--fixtures'].includes(argv[i]) && argv[i + 1] && !result[argv[i]], 'Expected --output-dir <target directory> [--connection-file <path>] [--fixtures <generated fixture directory>]')
    result[argv[i]] = argv[i + 1]
  }
  assert.ok(result['--output-dir'], '--output-dir is required')
  return result
}
function within(root, path) {
  const part = relative(root, path)
  return !isAbsolute(part) && part !== '..' && !part.startsWith(`..${sep}`)
}
async function fingerprint(path) {
  const info = await stat(path)
  assert.ok(info.isFile() && info.size < 512 * 1024 * 1024)
  return { path, size: info.size, mtimeMs: info.mtimeMs, sha256: createHash('sha256').update(await readFile(path)).digest('hex') }
}
const context = () => ({ projectId: session.project.projectId, revision: session.revision })
async function call(name, args = {}, phase = 'setup', started = performance.now()) {
  const begin = performance.now()
  const result = await client.callTool({ name, arguments: args }, undefined, { timeout: name === 'media_probe' ? 65000 : name.startsWith('project_') ? 35000 : name.startsWith('ui_') ? 3000 : 10000 })
  if (result.isError) throw new Error(`${name}: ${result.content[0]?.text ?? 'failed'}`)
  const value = JSON.parse(result.content[0].text)
  if (name.startsWith('ui_')) {
    const item = { phase, command: name, elapsedMs: performance.now() - started, roundTripMs: performance.now() - begin, ui: value }
    report.trace.push(item)
    assert.equal(value.nativeWindow?.visible, false, 'Requires an actually hidden --mcp-hidden window')
    assert.equal(value.nativeWindow?.focused, false, 'Validation must not take focus')
    return item
  }
  return value
}
function expected(frame) {
  return session.project.tracks.filter((track) => !track.muted).flatMap((track) => track.clips
    .filter((clip) => frame >= clip.startFrame && frame < clip.startFrame + clip.durationFrames)
    .map((clip) => ({ kind: track.kind, clipId: clip.id, mediaId: clip.mediaId, sourceTime: (clip.sourceInFrame + frame - clip.startFrame) / fps })))
}
function keys(nodes, actual = false) { return nodes.map((node) => `${node.kind}:${node.clipId}:${node.mediaId}${actual ? `:${node.nodeId}` : ''}`).sort() }
function mapped(ui) {
  const expectedNodes = expected(ui.playheadFrame)
  return ui.projectId === session.project.projectId && ui.revision === session.revision && ui.preview.mode === 'timeline'
    && JSON.stringify(keys(ui.mediaNodes)) === JSON.stringify(keys(expectedNodes))
    && ui.mediaNodes.every((node) => {
      const item = expectedNodes.find((entry) => entry.kind === node.kind && entry.clipId === node.clipId)
      return [node.currentTime, node.expectedSourceTime, node.driftSeconds, node.playbackRate].every(Number.isFinite)
        && Math.abs(node.expectedSourceTime - item.sourceTime) < 1e-6
        && Math.abs(node.driftSeconds - (node.currentTime - item.sourceTime)) < 1e-6
        && typeof node.nodeId === 'string' && node.nodeId.length > 0
        && Number.isSafeInteger(node.hardSeekCount) && node.hardSeekCount >= 0
    })
}
function ready(ui) { return mapped(ui) && ui.mediaNodes.every((node) => node.readyState >= 2 && !node.seeking && node.errorCode === null) }
function moving(ui) { return ready(ui) && ui.preview.playing && ui.mediaNodes.every((node) => !node.paused) }
function substantiveProgress(ui, startFrame) {
  return ui.playheadFrame >= startFrame + 3 && moving(ui) && expected(startFrame).every((initial) =>
    ui.mediaNodes.some((node) => node.kind === initial.kind && node.clipId === initial.clipId && node.currentTime >= initial.sourceTime + 2 / fps))
}
async function waitFor(predicate, label, phase, started = performance.now(), timeout = 3000) {
  const deadline = performance.now() + timeout
  let item
  while (performance.now() < deadline) {
    const tick = performance.now()
    item = await call('ui_inspect', {}, phase, started)
    if (predicate(item.ui)) return item
    await delay(Math.max(0, 65 - (performance.now() - tick)))
  }
  throw new Error(`${label} did not settle in ${timeout}ms; frame=${item?.ui.playheadFrame}, preview=${JSON.stringify(item?.ui.preview)}, nodes=${JSON.stringify(item?.ui.mediaNodes)}`)
}
async function update(name, args) {
  session = await call(name, { ...context(), ...args })
  await waitFor((ui) => ui.projectId === session.project.projectId && ui.revision === session.revision, 'UI project revision', 'setup')
}
async function seekPaused(frame, phase) {
  await call('ui_seek', { ...context(), frame }, phase)
  return waitFor((ui) => ui.playheadFrame === frame && !ui.preview.playing && ready(ui)
    && ui.mediaNodes.every((node) => node.paused && Math.abs(node.driftSeconds) < .002), 'Stopped seek', phase)
}
async function pauseCheck(phase) {
  const started = performance.now()
  await call('ui_pause', context(), phase, started)
  const first = await waitFor((ui) => !ui.preview.playing && ready(ui) && ui.mediaNodes.every((node) => node.paused && node.playbackRate === 1), 'Pause', phase, started)
  await delay(320)
  const second = await call('ui_inspect', {}, phase, started)
  assert.ok(ready(second.ui))
  assert.equal(second.ui.preview.playing, false)
  assert.equal(second.ui.playheadFrame, first.ui.playheadFrame)
  assert.deepEqual(keys(second.ui.mediaNodes, true), keys(first.ui.mediaNodes, true))
  for (const node of second.ui.mediaNodes) {
    assert.equal(node.paused, true)
    assert.equal(node.playbackRate, 1)
    assert.ok(Math.abs(node.currentTime - first.ui.mediaNodes.find((item) => item.nodeId === node.nodeId).currentTime) < .002, 'Paused media clock moved')
  }
  return { baseline: first, after320ms: second }
}
async function capture(phase, durationMs, startFrame, started, samples) {
  const end = started + durationMs
  while (performance.now() < end) {
    const tick = performance.now()
    samples.push(await call('ui_inspect', {}, phase, started))
    await delay(Math.max(0, 65 - (performance.now() - tick)))
  }
  const progressed = samples.find((item) => substantiveProgress(item.ui, startFrame))
  return { samples, progressed }
}
function checkSamples(samples, requireProgress = true) {
  let lastFrame = samples[0]?.ui.playheadFrame ?? 0
  for (const { ui } of samples) {
    assert.equal(ui.alerts.length, 0, 'Preview reported an error')
    assert.ok(ui.mediaNodes.every((node) => node.errorCode === null), 'Media decoder error')
    assert.ok(ui.playheadFrame >= lastFrame, 'Playback moved backwards without a seek')
    const nodes = expected(ui.playheadFrame)
    const master = nodes.find((node) => node.kind === 'audio') ?? nodes.find((node) => node.kind === 'video')
    assert.equal(ui.preview.clockMode, master?.kind ?? 'monotonic')
    assert.equal(ui.preview.masterClipId, master?.clipId ?? null)
    if (ui.mediaNodes.every((node) => node.readyState >= 2 && !node.seeking)) assert.ok(mapped(ui), 'Ready nodes have wrong identities or non-finite/inconsistent clocks')
    for (const node of ui.mediaNodes) if (node.clipId === master?.clipId) assert.equal(node.playbackRate, 1)
    lastFrame = ui.playheadFrame
  }
  if (requireProgress) assert.ok(samples.some((item) => moving(item.ui)), 'No ready unpaused playback sample')
}
function stats(values) {
  if (!values.length) return null
  assert.ok(values.every(Number.isFinite))
  const sorted = [...values].sort((a, b) => a - b)
  return { count: sorted.length, p95: sorted[Math.ceil(sorted.length * .95) - 1], max: sorted.at(-1), min: sorted[0] }
}
function startupTiming(samples, startFrame, originMs = 0) {
  const initial = expected(startFrame)
  const progresses = (ui, kind) => {
    const source = initial.find((node) => node.kind === kind)
    return !!source && ui.mediaNodes.some((node) => node.kind === kind && node.clipId === source.clipId && node.mediaId === source.mediaId
      && Number.isFinite(node.currentTime) && node.currentTime > source.sourceTime + .001
      && node.readyState >= 2 && !node.seeking && !node.paused && node.errorCode === null)
  }
  const first = (predicate) => {
    const item = samples.find(({ ui }) => predicate(ui))
    return item ? item.elapsedMs - originMs : null
  }
  const masterOnly = ({ ui }) => {
    const intended = expected(ui.playheadFrame)
    // An intended audio-only tail is not delayed video startup. Conversely,
    // a missing/paused video in an intended A/V interval must remain visible.
    if (!intended.some((node) => node.kind === 'audio') || !intended.some((node) => node.kind === 'video')) return false
    const active = (source) => ui.mediaNodes.some((node) => node.kind === source.kind && node.clipId === source.clipId && node.mediaId === source.mediaId
      && !node.paused && !node.seeking && node.readyState >= 2 && node.errorCode === null)
    const master = intended.find((node) => node.clipId === ui.preview.masterClipId)
    return !!master && active(master) && intended.some((node) => node.clipId !== master.clipId && !active(node))
  }
  let totalMs = 0, longestMs = 0, intervalStartedAt = null, masterOnlySamples = 0
  for (const item of samples) {
    if (masterOnly(item)) {
      masterOnlySamples++
      intervalStartedAt ??= item.elapsedMs
    } else if (intervalStartedAt !== null) {
      const elapsed = item.elapsedMs - intervalStartedAt
      totalMs += elapsed; longestMs = Math.max(longestMs, elapsed); intervalStartedAt = null
    }
  }
  if (intervalStartedAt !== null) {
    const elapsed = samples.at(-1).elapsedMs - intervalStartedAt
    totalMs += elapsed; longestMs = Math.max(longestMs, elapsed)
  }
  return { startFrame, originMs, sourceProgressThresholdSeconds: .001,
    firstVideoProgressMs: first((ui) => progresses(ui, 'video')),
    firstAudioProgressMs: first((ui) => progresses(ui, 'audio')),
    firstBothProgressMs: first((ui) => progresses(ui, 'video') && progresses(ui, 'audio')),
    firstUiFrameProgressMs: first((ui) => ui.playheadFrame > startFrame),
    firstFrameAndBothProgressMs: first((ui) => ui.playheadFrame > startFrame && progresses(ui, 'video') && progresses(ui, 'audio')),
    masterOnlyPlayingSamples: masterOnlySamples, estimatedMasterOnlyPlayingMs: totalMs, estimatedLongestMasterOnlyPlayingMs: longestMs,
    evidence: 'First progress is measured from the known initial source mapping, independently of playhead progress. Timing begins at ui_play/ui_seek request; boundary windows begin at first observed crossing. Master-only durations are polling estimates; unobserved time before the first sample is not invented.' }
}
function quality(samples, startFrame = 0, originMs = 0) {
  // Include every ready, unpaused pair from the first sample onward. No startup,
  // buffering or boundary grace filter is allowed to erase an excessive skew.
  const pairs = samples.filter(({ ui }) => moving(ui) && ui.mediaNodes.length === 2)
  const values = pairs.map(({ ui }) => {
    const audio = ui.mediaNodes.find((node) => node.kind === 'audio')
    const video = ui.mediaNodes.find((node) => node.kind === 'video')
    return Math.abs(audio.driftSeconds - video.driftSeconds)
  })
  const distribution = stats(values)
  return { status: !distribution ? 'unavailable' : distribution.p95 < 1 / fps && distribution.max < 1 / fps ? 'passed' : 'failed',
    relativeClockSeconds: distribution, oneFrameSeconds: 1 / fps, totalSamples: samples.length, pairedSamples: pairs.length,
    startupTiming: startupTiming(samples, startFrame, originMs),
    bufferingSamples: samples.filter(({ ui }) => ui.preview.buffering === true).length,
    readyButPausedWhilePlaying: samples.filter(({ ui }) => ui.preview.playing && ready(ui) && ui.mediaNodes.some((node) => node.paused)).length }
}
async function runCase(name, action) {
  const entry = { name, startFrame: name.startsWith('playing-seek-') ? Number(name.slice('playing-seek-'.length)) : 0, status: 'running', samples: [] }
  report.cases.push(entry)
  try { await action(entry); entry.status = 'passed' } catch (error) { entry.status = 'failed'; entry.error = error.message }
  finally {
    entry.quality = quality(entry.samples, entry.startFrame)
    entry.first500ms = quality(entry.samples.filter((item) => item.elapsedMs <= 500), entry.startFrame)
    if (name === 'all-boundaries') entry.boundaries = [60, 120, 180, 240, 300, 360, 420].map((frame) => {
      const crossing = entry.samples.find(({ ui }) => ui.playheadFrame >= frame)
      const window = crossing ? entry.samples.filter((item) => item.elapsedMs >= crossing.elapsedMs && item.elapsedMs <= crossing.elapsedMs + 500) : []
      const recovered = crossing && entry.samples.find((item) => item.elapsedMs >= crossing.elapsedMs && ready(item.ui)
        && item.ui.preview.buffering !== true && item.ui.mediaNodes.every((node) => !item.ui.preview.playing || !node.paused))
      return { frame, firstObservedMs: crossing?.elapsedMs ?? null, recoveredAfterCrossingMs: recovered ? recovered.elapsedMs - crossing.elapsedMs : null,
        first500ms: quality(window, frame, crossing?.elapsedMs ?? 0), samples: window }
    })
    try { entry.pause = await pauseCheck(`${name}:pause`) } catch (error) { entry.status = 'failed'; entry.pauseError = error.message }
    console.log(JSON.stringify({ case: name, status: entry.status, error: entry.error, progressMs: entry.progressMs, quality: entry.quality }))
  }
}

try {
  const args = options(), output = resolve(args['--output-dir'])
  assert.ok(within(target, output), 'Output must stay under src-tauri/target')
  await mkdir(output, { recursive: true })
  assert.ok(within(await realpath(target), await realpath(output)), 'Output must not escape through links')
  directory = join(output, `run-${randomUUID()}`); await mkdir(directory)
  report.directory = directory
  const fixtures = resolve(args['--fixtures'] ?? defaultFixtures)
  const paths = ['610秒动态画面.mp4', '610秒节拍.wav'].map((name) => join(fixtures, name))
  sources.push(...await Promise.all(paths.map(fingerprint)))
  await client.connect(new StdioClientTransport({ command: process.execPath,
    args: [join(workspace, 'mcp', 'server.mjs'), '--connection-file', connectionFilePath(args['--connection-file'] ? ['--connection-file', args['--connection-file']] : [])], stderr: 'inherit', maxBufferSize: 17 * 1024 * 1024 }))
  connected = true
  assert.equal((await client.listTools()).tools.length, 20)
  session = await call('get_state')
  assert.ok(!session.path && !session.dirty && !session.canUndo && !session.canRedo && !session.project.mediaRefs.length && session.project.tracks.every((track) => !track.clips.length), 'Use a fresh isolated empty validation instance')
  await call('ui_inspect')
  await update('project_import_paths', { paths })
  const probes = await call('media_probe')
  assert.ok(probes.length === 2 && probes.every((probe) => probe.state === 'ready' && Math.abs(probe.durationMs - 610000) < 100))
  await update('project_save_as', { path: join(directory, '测试素材.kaicut') })
  const project = structuredClone(session.project)
  project.projectId = `project-${randomUUID()}`; project.name = '起播与边界一致性验证'
  for (const track of project.tracks) track.clips = []
  for (const [kind, id, start, sourceIn, duration] of definitions) {
    const track = project.tracks.find((item) => item.kind === kind), media = project.mediaRefs.find((item) => item.kind === kind)
    track.clips.push({ id, mediaId: media.id, trackId: track.id, startFrame: start * fps, sourceInFrame: sourceIn * fps, durationFrames: duration * fps, sourceDurationFrames: 610 * fps, fadeInFrames: 0, fadeOutFrames: 0 })
  }
  report.projectPath = join(directory, '起播验证.kaicut')
  await writeFile(report.projectPath, JSON.stringify(project, null, 2), { flag: 'wx' })
  await update('project_open_path', { path: report.projectPath })

  for (let repeat = 1; repeat <= 5; repeat++) await runCase(`startup-${repeat}`, async (entry) => {
    await seekPaused(0, `${entry.name}:seek`)
    const started = performance.now()
    entry.samples.push(await call('ui_play', context(), entry.name, started))
    const result = await capture(entry.name, 1900, 0, started, entry.samples)
    if (!result.progressed) entry.samples.push(await waitFor((ui) => substantiveProgress(ui, 0), 'Startup progress', entry.name, started, Math.max(1, 3000 - (performance.now() - started))))
    const progressed = result.progressed ?? entry.samples.at(-1)
    entry.progressMs = progressed.elapsedMs
    assert.ok(entry.progressMs <= 3000, 'Startup did not make substantive progress within three seconds')
    checkSamples(entry.samples)
  })

  await runCase('all-boundaries', async (entry) => {
    await seekPaused(0, 'all-boundaries:seek')
    const started = performance.now()
    entry.samples.push(await call('ui_play', context(), entry.name, started))
    let stillSince = started, lastFrame = 0
    while (performance.now() - started < 20000) {
      const tick = performance.now(), item = await call('ui_inspect', {}, entry.name, started)
      entry.samples.push(item)
      if (item.ui.playheadFrame > lastFrame) stillSince = performance.now()
      assert.ok(performance.now() - stillSince < 3000, 'Boundary playback made no progress for three seconds')
      lastFrame = item.ui.playheadFrame
      if (!item.ui.preview.playing) break
      await delay(Math.max(0, 65 - (performance.now() - tick)))
    }
    checkSamples(entry.samples)
    assert.equal(entry.samples.at(-1).ui.playheadFrame, 420)
    assert.equal(entry.samples.at(-1).ui.preview.playing, false)
    for (const kind of ['video', 'audio']) {
      const a = entry.samples.find(({ ui }) => ui.playheadFrame >= 30 && ui.playheadFrame < 50 && moving(ui))?.ui.mediaNodes.find((node) => node.kind === kind)
      const b = entry.samples.find(({ ui }) => ui.playheadFrame >= 75 && ui.playheadFrame < 105 && moving(ui))?.ui.mediaNodes.find((node) => node.kind === kind)
      const c = entry.samples.find(({ ui }) => ui.playheadFrame >= 135 && ui.playheadFrame < 165 && moving(ui))?.ui.mediaNodes.find((node) => node.kind === kind)
      assert.ok(a && b && c)
      assert.equal(a.nodeId, b.nodeId); assert.equal(b.nodeId, c.nodeId)
      assert.equal(a.hardSeekCount, b.hardSeekCount)
      assert.ok(c.hardSeekCount >= b.hardSeekCount + 1)
    }
    for (const [start, end, kind] of [[195, 225, null], [315, 345, 'audio'], [375, 405, 'video']]) {
      assert.ok(entry.samples.some(({ ui }) => ui.playheadFrame >= start && ui.playheadFrame < end && moving(ui)
        && (kind ? ui.mediaNodes.length === 1 && ui.mediaNodes[0].kind === kind : ui.mediaNodes.length === 0)), `Missing ${kind ?? 'gap'} interval`)
    }
  })

  for (const frame of [120, 240, 30]) await runCase(`playing-seek-${frame}`, async (entry) => {
    await seekPaused(0, `${entry.name}:reset`)
    await call('ui_play', context(), `${entry.name}:before`)
    await waitFor((ui) => substantiveProgress(ui, 0), 'Playback before seek', `${entry.name}:before`)
    const started = performance.now()
    entry.samples.push(await call('ui_seek', { ...context(), frame }, entry.name, started))
    const result = await capture(entry.name, 1900, frame, started, entry.samples)
    if (!result.progressed) entry.samples.push(await waitFor((ui) => substantiveProgress(ui, frame), 'Running seek progress', entry.name, started, Math.max(1, 3000 - (performance.now() - started))))
    entry.progressMs = (result.progressed ?? entry.samples.at(-1)).elapsedMs
    assert.ok(entry.progressMs <= 3000)
    checkSamples(entry.samples)
  })
  assert.equal((await call('get_state')).dirty, false)
} catch (error) { report.error = error.message }
finally {
  if (connected && session) try { await call('ui_pause', context(), 'cleanup') } catch (error) { report.cleanupError = error.message }
  try { for (const original of sources) assert.deepEqual(await fingerprint(original.path), original); report.sources = sources } catch (error) { report.integrityError = error.message }
  await client.close()
  report.repeatedStarts = { requested: 5, completed: report.cases.filter((item) => item.name.startsWith('startup-')).length,
    passed: report.cases.filter((item) => item.name.startsWith('startup-') && item.status === 'passed').length }
  report.functional = { status: !report.error && !report.cleanupError && !report.integrityError && report.cases.length === 9 && report.cases.every((item) => item.status === 'passed') ? 'passed' : 'failed' }
  report.quality = { status: report.cases.length === 9 && report.cases.every((item) => item.quality?.status === 'passed') ? 'passed' : 'failed',
    target: 'P95 and maximum relative audio/video media-clock skew below one 30 fps frame for every observed ready/unpaused pair, including startup and boundaries.' }
  report.ok = report.functional.status === 'passed' && report.quality.status === 'passed'
  if (!report.ok) process.exitCode = 1
  if (directory) await writeFile(join(directory, 'startup-validation.json'), JSON.stringify(report, null, 2), { flag: 'wx' })
  console.log(JSON.stringify({ ok: report.ok, directory, error: report.error, functional: report.functional, quality: report.quality, repeatedStarts: report.repeatedStarts,
    cases: report.cases.map(({ samples: _samples, pause: _pause, boundaries: _boundaries, ...item }) => item) }, null, 2))
}
