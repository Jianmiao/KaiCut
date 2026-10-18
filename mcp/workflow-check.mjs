#!/usr/bin/env node
import assert from 'node:assert/strict'
import { createHash, randomUUID } from 'node:crypto'
import { copyFile, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'
import { fileURLToPath } from 'node:url'
import { Client } from '@modelcontextprotocol/sdk/client/index.js'
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js'
import { connectionFilePath } from './bridge.mjs'
import { validateExportWorkflow } from './export-check.mjs'

const workspace = dirname(dirname(fileURLToPath(import.meta.url)))
const target = join(workspace, 'src-tauri', 'target')
const defaultFixtures = join(target, 'workflow-validation', '98ee1285-41b7-452a-bbce-e38c83b1a2aa')
const fixtureNames = ['原始 视频;[literal].mp4', '原始 音频.wav', '原始 图片%03d.png']
const client = new Client({ name: 'kaicut-workflow-check', version: '0.1.0' })
const checks = []
const report = { ok: false, checks, evidence: 'Real MCP commands and live WebView media state. No pointer control, screenshots or pixel equivalence claim.' }
let runDirectory
let currentSession
let sources = []
let copies = []

function options() {
  const output = {}
  const args = process.argv.slice(2)
  for (let i = 0; i < args.length; i++) {
    const key = args[i]
    if (key === '--export' && !output[key]) { output[key] = true; continue }
    if (!['--connection-file', '--output-dir', '--fixtures'].includes(key) || !args[i + 1] || output[key]) throw new Error('Usage: node mcp/workflow-check.mjs --output-dir <directory under src-tauri/target> [--connection-file <path>] [--fixtures <generated fixture directory>] [--export]')
    output[key] = args[++i]
  }
  assert.ok(output['--output-dir'], '--output-dir is required')
  return output
}

function within(base, candidate) {
  const path = relative(base, candidate)
  return path === '' || (path !== '..' && !path.startsWith(`..${sep}`) && !isAbsolute(path))
}

async function fingerprint(path) {
  const info = await stat(path)
  assert.ok(info.isFile(), 'Fixture must be a file')
  assert.ok(info.size < 32 * 1024 * 1024, 'Fixture size must stay below 32 MiB')
  return { path, size: info.size, mtimeMs: info.mtimeMs, sha256: createHash('sha256').update(await readFile(path)).digest('hex') }
}

async function call(name, args = {}) {
  const result = await client.callTool({ name, arguments: args }, undefined, { timeout: ['media_probe', 'export_preflight', 'export_prepare'].includes(name) ? 65000 : name.startsWith('project_') ? 35000 : 10000 })
  if (result.isError) throw new Error(`${name}: ${result.content[0]?.text || 'failed'}`)
  const value = JSON.parse(result.content[0].text)
  if (name.startsWith('ui_')) assert.equal(value.nativeWindow?.visible, false, 'Workflow validation requires --mcp-hidden and a genuinely hidden native window; this script will not hide or repair the window')
  return value
}

function context(session = currentSession) { return { projectId: session.project.projectId, revision: session.revision } }
async function update(name, args) {
  currentSession = await call(name, { ...context(), ...args })
  await inspectUntil((ui) => ui.projectId === currentSession.project.projectId && ui.revision === currentSession.revision, 'UI revision synchronization')
  return currentSession
}
async function edit(change) { return update('timeline_edit', { edit: change }) }

async function inspectUntil(predicate, description, timeoutMs = 10000) {
  const deadline = Date.now() + timeoutMs
  let ui
  while (Date.now() < deadline) {
    ui = await call('ui_inspect')
    if (predicate(ui)) return ui
    await delay(150)
  }
  throw new Error(`${description} did not complete within ${timeoutMs} ms; last frame=${ui?.playheadFrame}, playing=${ui?.preview?.playing}, viewport=${JSON.stringify(ui?.viewport)}, nativeWindow=${JSON.stringify(ui?.nativeWindow)}, nodes=${JSON.stringify(ui?.mediaNodes)}, images=${JSON.stringify(ui?.imageNodes)}`)
}

async function rejectsWithoutChange(name, args, description) {
  const before = await call('get_state')
  const result = await client.callTool({ name, arguments: args }, undefined, { timeout: 35000 })
  assert.equal(result.isError, true, description)
  assert.deepEqual(await call('get_state'), before, `${description}: rejected operation changed session`)
  checks.push(description)
}

try {
  const args = options()
  const output = resolve(args['--output-dir'])
  const actualTarget = await realpath(target)
  assert.ok(within(resolve(target), output), '--output-dir must remain under this workspace src-tauri/target')
  await mkdir(output, { recursive: true })
  assert.ok(within(actualTarget, await realpath(output)), '--output-dir must not resolve through a link outside src-tauri/target')
  runDirectory = join(output, `run-${randomUUID()}`)
  await mkdir(runDirectory)
  report.directory = runDirectory
  const fixtures = resolve(args['--fixtures'] || defaultFixtures)
  sources = await Promise.all(fixtureNames.map((name) => fingerprint(join(fixtures, name))))
  for (const source of sources) {
    const destination = join(runDirectory, relative(fixtures, source.path))
    await copyFile(source.path, destination)
    copies.push(await fingerprint(destination))
  }
  assert.deepEqual(copies.map((item) => item.sha256), sources.map((item) => item.sha256))
  checks.push('Copied three generated media fixtures without changing originals')

  const connectionArgs = args['--connection-file'] ? ['--connection-file', args['--connection-file']] : []
  await client.connect(new StdioClientTransport({
    command: process.execPath,
    args: [join(workspace, 'mcp', 'server.mjs'), '--connection-file', connectionFilePath(connectionArgs)],
    stderr: 'inherit',
    maxBufferSize: 17 * 1024 * 1024,
  }))
  currentSession = await call('get_state')
  const initialUi = await call('ui_inspect')
  report.initialWindow = { viewport: initialUi.viewport, nativeWindow: initialUi.nativeWindow }
  checks.push('Rust confirms the native validation window is hidden (--mcp-hidden)')
  assert.equal(currentSession.path, null, 'Use a disposable unsaved --mcp-validation app')
  assert.equal(currentSession.dirty, false, 'Validation must start from a clean project')
  assert.equal(currentSession.project.mediaRefs.length, 0, 'Validation must never use personal media')
  assert.ok(currentSession.project.tracks.every((track) => track.clips.length === 0), 'Validation requires an empty timeline')
  assert.equal(currentSession.canUndo, false, 'Validation requires empty history')
  const mediaPaths = copies.map((item) => item.path)
  await rejectsWithoutChange('project_import_paths', { ...context(), revision: currentSession.revision + 1, paths: mediaPaths }, 'Stale import rejected atomically')
  await update('project_import_paths', { paths: mediaPaths })
  assert.equal(currentSession.project.mediaRefs.length, 3)
  const probes = await call('media_probe')
  assert.equal(probes.length, 3)
  assert.ok(probes.every((probe) => probe.state === 'ready'), 'All three real media files must probe successfully')
  const refs = currentSession.project.mediaRefs
  const video = refs.find((asset) => asset.kind === 'video')
  const audio = refs.find((asset) => asset.kind === 'audio')
  const image = refs.find((asset) => asset.kind === 'image')
  assert.ok(video && audio && image)
  checks.push('Real import and sidecar probe of MP4, WAV and PNG')

  const videoTrackId = currentSession.project.tracks.find((track) => track.kind === 'video').id
  const imageTrackId = currentSession.project.tracks.filter((track) => track.kind === 'video')[1].id
  const audioTrackId = currentSession.project.tracks.find((track) => track.kind === 'audio').id
  const track = (id) => currentSession.project.tracks.find((item) => item.id === id)
  await edit({ type: 'add', mediaId: video.id, trackId: videoTrackId })
  const videoClipId = track(videoTrackId).clips[0].id
  await edit({ type: 'trim', clipId: videoClipId, sourceInFrame: 30, startFrame: 0, durationFrames: 60 })
  await edit({ type: 'split', clipId: videoClipId, frame: 30 })
  const leftClip = track(videoTrackId).clips.find((clip) => clip.startFrame === 0)
  const rightClip = track(videoTrackId).clips.find((clip) => clip.startFrame === 30)
  await edit({ type: 'move', clipId: rightClip.id, trackId: videoTrackId, startFrame: 60 })
  await edit({ type: 'add', mediaId: image.id, trackId: imageTrackId })
  const imageClipId = track(imageTrackId).clips[0].id
  await edit({ type: 'trim', clipId: imageClipId, sourceInFrame: 0, startFrame: 90, durationFrames: 30 })
  await edit({ type: 'add', mediaId: audio.id, trackId: audioTrackId })
  const audioClipId = track(audioTrackId).clips[0].id
  await edit({ type: 'trackVolume', trackId: audioTrackId, volume: 0.5 })
  await edit({ type: 'clipFade', clipId: audioClipId, fadeInFrames: 15, fadeOutFrames: 15 })
  checks.push('Real timeline add, source trim, split, move, image placement, volume and fades')

  const fps = currentSession.project.timebase.fpsNumerator / currentSession.project.timebase.fpsDenominator
  assert.equal(fps, 30, 'Generated fixture expectations use a 30 fps project')
  await call('ui_seek', { ...context(), frame: 15 })
  const sourceView = await inspectUntil((ui) => ui.preview.visualClipId === leftClip.id && ui.mediaNodes.some((node) => node.clipId === leftClip.id && node.kind === 'video' && node.readyState >= 2 && !node.seeking && Math.abs(node.currentTime - 1.5) < 0.15), 'Trimmed source-time preview')
  assert.ok(sourceView.mediaNodes.every((node) => node.errorCode === null))
  const audioNode = sourceView.mediaNodes.find((node) => node.clipId === audioClipId)
  assert.ok(audioNode, 'The audio track must reach the live player')
  assert.ok(Math.abs(audioNode.volume - 0.5) < 0.01)
  report.trimmedPreview = { frame: sourceView.playheadFrame, mediaNodes: sourceView.mediaNodes, viewport: sourceView.viewport, nativeWindow: sourceView.nativeWindow }
  await call('ui_seek', { ...context(), frame: 45 })
  const gapView = await inspectUntil((ui) => ui.playheadFrame === 45 && ui.preview.visualClipId === null && ui.preview.audioClipIds.includes(audioClipId), 'Timeline gap with continuing audio')
  report.gapPreview = { frame: gapView.playheadFrame, preview: gapView.preview, viewport: gapView.viewport, nativeWindow: gapView.nativeWindow }
  await call('ui_seek', { ...context(), frame: 105 })
  const imageView = await inspectUntil((ui) => ui.preview.visualClipId === imageClipId && ui.imageNodes?.some((node) => node.complete && node.naturalWidth > 0 && node.naturalHeight > 0), 'Decoded image clip preview')
  report.imagePreview = { frame: imageView.playheadFrame, imageNodes: imageView.imageNodes, viewport: imageView.viewport, nativeWindow: imageView.nativeWindow }
  checks.push('Live decoded video source time, audio gain, gap and successfully loaded image')

  const firstProjectPath = join(runDirectory, 'MCP 初次保存.kaicut')
  await update('project_save_as', { path: firstProjectPath })
  assert.equal(currentSession.dirty, false)
  assert.deepEqual(JSON.parse(await readFile(firstProjectPath, 'utf8')).tracks, currentSession.project.tracks)
  const savedFingerprint = await fingerprint(firstProjectPath)
  await rejectsWithoutChange('project_save_as', { ...context(), path: firstProjectPath }, 'Save-as refuses existing destination without changing session')
  assert.deepEqual(await fingerprint(firstProjectPath), savedFingerprint)
  await edit({ type: 'trackVolume', trackId: audioTrackId, volume: 0.4 })
  await rejectsWithoutChange('project_open_path', { ...context(), path: firstProjectPath }, 'Opening a project refuses to discard unsaved edits')
  await update('timeline_undo', {})
  const savedProjectPath = join(runDirectory, 'MCP 剪辑验收.kaicut')
  await update('project_save_as', { path: savedProjectPath })
  const expectedProject = structuredClone(currentSession.project)
  await call('ui_select', { ...context(), clipIds: [leftClip.id] })
  await update('project_open_path', { path: savedProjectPath })
  assert.equal(currentSession.project.projectId, expectedProject.projectId)
  assert.deepEqual(currentSession.project.tracks, expectedProject.tracks)
  assert.equal(currentSession.dirty, false)
  assert.equal(currentSession.canUndo, false)
  const reopened = await inspectUntil((ui) => ui.selectedClipIds.length === 0 && ui.playheadFrame === 0 && ui.preview.mode === 'timeline', 'Reopening the same project clears selection/playhead')
  report.reopened = { revision: reopened.revision, frame: reopened.playheadFrame, selectedClipIds: reopened.selectedClipIds, viewport: reopened.viewport, nativeWindow: reopened.nativeWindow }
  checks.push('Save, overwrite refusal, dirty-open refusal, undo and reopening the same project')

  await call('ui_seek', { ...context(), frame: 0 })
  const playbackClipIds = [leftClip.id, audioClipId]
  const beforePlayback = await inspectUntil((ui) => ui.playheadFrame === 0 && playbackClipIds.every((clipId) => ui.mediaNodes.some((node) => node.clipId === clipId && node.readyState >= 2 && !node.seeking && node.paused)), 'Video/audio readiness after reopening')
  const initialMedia = new Map(beforePlayback.mediaNodes.map((node) => [node.clipId, node]))
  let advanced
  let pausedView
  try {
    const playing = await call('ui_play', context())
    assert.equal(playing.preview.playing, true)
    const provesPlayback = (ui) => ui.preview.playing && ui.playheadFrame >= 5
      // This deliberately samples the first continuous video segment [0,30).
      // A later node is a different clip, not evidence of this node advancing.
      && ui.playheadFrame < leftClip.startFrame + leftClip.durationFrames
      && playbackClipIds.every((clipId) => ui.mediaNodes.some((node) => node.clipId === clipId
        && node.kind === initialMedia.get(clipId).kind && !node.paused && !node.seeking
        && node.readyState >= 2 && node.errorCode === null
        && node.currentTime - initialMedia.get(clipId).currentTime >= 2 / fps))
    advanced = provesPlayback(playing) ? playing : await inspectUntil(provesPlayback, 'Video and audio currentTime advancing while unpaused in the same clip interval', 8000)
  } finally {
    pausedView = await call('ui_pause', context())
    assert.equal(pausedView.preview.playing, false)
  }
  // Pausing may occur on a clip boundary; settle on whatever nodes are actually
  // active after pause rather than comparing a previous clip's currentTime.
  const pauseBaseline = await inspectUntil((ui) => !ui.preview.playing && ui.mediaNodes.every((node) => node.paused && !node.seeking), 'Paused media nodes settle')
  await delay(300)
  const pauseAfter = await call('ui_inspect')
  assert.equal(pauseAfter.preview.playing, false)
  assert.ok(Math.abs(pauseAfter.playheadFrame - pauseBaseline.playheadFrame) <= 1, 'Paused playhead must remain stable within one frame')
  assert.deepEqual(pauseAfter.mediaNodes.map((node) => [node.kind, node.clipId]), pauseBaseline.mediaNodes.map((node) => [node.kind, node.clipId]), 'Paused active nodes must remain unchanged')
  for (const node of pauseAfter.mediaNodes) {
    const baseline = pauseBaseline.mediaNodes.find((item) => item.kind === node.kind && item.clipId === node.clipId)
    assert.equal(node.paused, true, 'Paused media must report paused=true')
    assert.equal(node.seeking, false, 'Paused media must have settled source time')
    assert.ok(Math.abs(node.currentTime - baseline.currentTime) <= 1 / fps, 'Paused media currentTime must remain stable within one frame')
  }
  report.playback = {
    before: { frame: beforePlayback.playheadFrame, mediaNodes: beforePlayback.mediaNodes, viewport: beforePlayback.viewport, nativeWindow: beforePlayback.nativeWindow },
    advanced: { frame: advanced.playheadFrame, mediaNodes: advanced.mediaNodes, viewport: advanced.viewport, nativeWindow: advanced.nativeWindow },
    pauseAcknowledgedAtFrame: pausedView.playheadFrame,
    pauseBaseline: { frame: pauseBaseline.playheadFrame, mediaNodes: pauseBaseline.mediaNodes, viewport: pauseBaseline.viewport, nativeWindow: pauseBaseline.nativeWindow },
    pauseAfter300ms: { frame: pauseAfter.playheadFrame, mediaNodes: pauseAfter.mediaNodes, viewport: pauseAfter.viewport, nativeWindow: pauseAfter.nativeWindow },
  }
  checks.push('Live video/audio source times advance while unpaused; playhead and media times remain stable after pause')
  report.projectPath = savedProjectPath
  if (args['--export']) await validateExportWorkflow({ call, update, context, runDirectory, workspace, report })
  report.ok = true
} catch (error) {
  report.error = error instanceof Error ? error.message : 'MCP media workflow failed'
  process.exitCode = 1
} finally {
  try {
    for (const item of [...sources, ...copies]) assert.deepEqual(await fingerprint(item.path), item, 'Media source changed during workflow')
    if (sources.length && copies.length) checks.push('Original and copied media hashes, sizes and modification times unchanged')
  } catch (error) {
    report.ok = false
    report.error = error instanceof Error ? error.message : 'Source integrity verification failed'
    process.exitCode = 1
  }
  await client.close()
  if (runDirectory) {
    report.sources = copies
    await writeFile(join(runDirectory, 'validation.json'), `${JSON.stringify(report, null, 2)}\n`, { flag: 'wx' })
  }
  console.log(JSON.stringify(report, null, 2))
}
