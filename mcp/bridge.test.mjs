import assert from 'node:assert/strict'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test from 'node:test'
import { connectionFilePath, requestBridge } from './bridge.mjs'

async function stalledBridge(t) {
  const directory = await mkdtemp(join(tmpdir(), 'kaicut-bridge-'))
  const sockets = new Set()
  const server = createServer((socket) => {
    sockets.add(socket)
    socket.on('close', () => sockets.delete(socket))
    socket.on('data', () => {})
  })
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
  const path = join(directory, 'connection.json')
  await writeFile(path, JSON.stringify({ version: 1, port: server.address().port, token: 'b'.repeat(64), pid: process.pid }))
  t.after(async () => {
    for (const socket of sockets) socket.destroy()
    await new Promise((resolve) => server.close(resolve))
    await rm(directory, { recursive: true, force: true })
  })
  return path
}

test('a stalled bridge times out and tells clients to inspect before retrying mutations', async (t) => {
  const path = await stalledBridge(t)
  await assert.rejects(requestBridge(path, 'timeline_undo', { projectId: 'p', revision: 1 }, { timeoutMs: 40 }), /timed out.*re-read state/)
})

test('client cancellation closes a pending bridge call without retrying it', async (t) => {
  const path = await stalledBridge(t)
  const abort = new AbortController()
  const request = requestBridge(path, 'ui_inspect', {}, { signal: abort.signal })
  const timeout = setTimeout(() => abort.abort(), 40)
  try { await assert.rejects(request, /cancelled.*re-read state/) } finally { clearTimeout(timeout) }
})

test('export timeout and request cancellation require job-status checks rather than claiming the job stopped', async (t) => {
  const path = await stalledBridge(t)
  await assert.rejects(requestBridge(path, 'export_prepare', { projectId: 'p', revision: 1, outputPath: 'E:\\out.mp4' }, { timeoutMs: 40 }), /timed out.*does not cancel the export job.*export_job_list\/export_job_status/)
  const abort = new AbortController()
  const request = requestBridge(path, 'export_job_start', { jobId: 'job-123' }, { signal: abort.signal })
  const timer = setTimeout(() => abort.abort(), 40)
  try { await assert.rejects(request, /cancelled.*does not cancel the export job.*never blindly retry/) } finally { clearTimeout(timer) }
})

test('bridge request limit rejects a large edit before connecting', async (t) => {
  const path = await stalledBridge(t)
  await assert.rejects(requestBridge(path, 'timeline_edit', { data: 'x'.repeat(1024 * 1024) }), /1 MiB/)
})

test('explicit connection file wins over environment and malformed CLI arguments fail closed', () => {
  assert.equal(connectionFilePath(['--connection-file', 'chosen.json'], { KAICUT_MCP_CONNECTION_FILE: 'env.json' }), 'chosen.json')
  assert.equal(connectionFilePath([], { KAICUT_MCP_CONNECTION_FILE: 'env.json' }), 'env.json')
  assert.throws(() => connectionFilePath(['--host', 'example.com']), /Usage/)
})
