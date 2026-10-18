#!/usr/bin/env node
import assert from 'node:assert/strict'
import { setTimeout as delay } from 'node:timers/promises'
import { fileURLToPath } from 'node:url'
import { Client } from '@modelcontextprotocol/sdk/client/index.js'
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js'
import { connectionFilePath } from './bridge.mjs'

const argumentsList = process.argv.slice(2)
const exercise = argumentsList.includes('--exercise')
const connectionArgs = argumentsList.filter((item) => item !== '--exercise')
const client = new Client({ name: 'kaicut-live-check', version: '0.1.0' })
const checks = []

async function call(name, args = {}) {
  const result = await client.callTool({ name, arguments: args }, undefined, { timeout: name === 'media_probe' ? 65000 : 10000 })
  if (result.isError) throw new Error(result.content[0]?.text || `${name} failed`)
  return JSON.parse(result.content[0].text)
}

function context(session) {
  return { projectId: session.project.projectId, revision: session.revision }
}

async function inspectAtRevision(revision) {
  for (let i = 0; i < 20; i += 1) {
    const ui = await call('ui_inspect')
    if (ui.revision === revision) return ui
    await delay(100)
  }
  throw new Error('The rendered UI did not adopt the authoritative project revision.')
}

try {
  await client.connect(new StdioClientTransport({
    command: process.execPath,
    args: [fileURLToPath(new URL('./server.mjs', import.meta.url)), '--connection-file', connectionFilePath(connectionArgs)],
    stderr: 'inherit',
    maxBufferSize: 17 * 1024 * 1024,
  }))
  await client.ping()
  const { tools } = await client.listTools()
  assert.ok(tools.some((tool) => tool.name === 'timeline_edit'))
  checks.push('MCP handshake, ping and tools/list')

  const before = await call('get_state')
  const ui = await inspectAtRevision(before.revision)
  assert.equal(ui.projectId, before.project.projectId)
  assert.ok(Array.isArray(ui.controls))
  assert.ok(ui.viewport.width > 0 && ui.viewport.height > 0)
  checks.push('Rust session and rendered UI agree on project/revision')
  const links = await call('media_check_links')
  const probes = await call('media_probe')
  assert.ok(Array.isArray(links))
  assert.ok(Array.isArray(probes))
  const jobs = await call('export_job_list')
  assert.ok(Array.isArray(jobs))
  if (jobs.length) {
    const job = await call('export_job_status', { jobId: jobs[0].jobId })
    assert.equal(job.jobId, jobs[0].jobId)
  }
  checks.push('Media links, metadata and export-job inspection')

  if (exercise) {
    assert.equal(before.path, null, '--exercise requires a disposable unsaved project')
    assert.equal(before.dirty, false, '--exercise requires a clean project')
    assert.equal(before.canUndo, false, '--exercise requires an empty edit history')
    assert.equal(before.canRedo, false, '--exercise requires an empty redo history')
    assert.equal(before.project.mediaRefs.length, 0, '--exercise never uses imported personal media')
    assert.ok(before.project.tracks.every((track) => track.clips.length === 0), '--exercise requires an empty timeline')
    const track = before.project.tracks[0]
    assert.ok(track, 'At least one track is required')
    const edit = { type: 'trackState', trackId: track.id, locked: !track.locked, muted: track.muted }
    const stale = await client.callTool({ name: 'timeline_edit', arguments: { ...context(before), revision: before.revision + 1, edit } })
    assert.equal(stale.isError, true, 'An incorrect revision must reject the edit')
    const unchanged = await call('get_state')
    assert.deepEqual(unchanged, before, 'Stale edit must not change project/session')
    checks.push('Stale revision rejected without changing project/session')

    const paused = await call('ui_pause', context(before))
    assert.equal(paused.preview.playing, false)
    const seek = await call('ui_seek', { ...context(before), frame: 0 })
    assert.equal(seek.playheadFrame, 0)
    const selection = await call('ui_select', { ...context(before), clipIds: [] })
    assert.deepEqual(selection.selectedClipIds, [])
    checks.push('Pause, seek to frame zero and clear selection acknowledged by rendered UI')

    const edited = await call('timeline_edit', { ...context(before), edit })
    try {
      assert.equal(edited.project.tracks[0].locked, !track.locked)
      assert.ok(edited.revision > before.revision)
      await inspectAtRevision(edited.revision)
    } finally {
      // Undo only our acknowledged edit. A concurrent human edit makes this fail
      // the native revision check instead of undoing somebody else's work.
      const restored = await call('timeline_undo', context(edited))
      assert.deepEqual(restored.project.tracks, before.project.tracks)
      await inspectAtRevision(restored.revision)
    }
    checks.push('Track-lock edit reaches UI; undo restores original timeline content')
  }
  console.log(JSON.stringify({
    ok: true, exercise, checks,
    toolCount: tools.length, mediaCount: links.length, exportJobCount: jobs.length,
    note: exercise ? 'Timeline content restored; revision/history advance and undo intentionally leaves dirty=true. Close this disposable project without saving.' : 'Read-only checks. No project edit, playback or export was started.',
  }, null, 2))
} catch (error) {
  console.error(error instanceof Error ? error.message : 'KaiCut live MCP check failed.')
  process.exitCode = 1
} finally {
  await client.close()
}
