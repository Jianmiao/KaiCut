import assert from 'node:assert/strict'
import { createServer } from 'node:net'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import test from 'node:test'
import { Client } from '@modelcontextprotocol/sdk/client/index.js'
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js'

const entry = fileURLToPath(new URL('./server.mjs', import.meta.url))
const secret = 'a'.repeat(64)

async function fixture(t, responder = () => ({ ok: true, result: {} })) {
  const folder = await mkdtemp(join(tmpdir(), 'kaicut-mcp-'))
  const calls = []
  const sockets = new Set()
  const bridge = createServer((socket) => {
    sockets.add(socket)
    socket.on('close', () => sockets.delete(socket))
    let text = ''
    socket.on('data', (chunk) => {
      text += chunk.toString('utf8')
      if (!text.includes('\n')) return
      const request = JSON.parse(text.slice(0, text.indexOf('\n')))
      calls.push(request)
      const response = responder(request, socket)
      if (response !== undefined) socket.end(`${JSON.stringify(response)}\n`)
    })
  })
  await new Promise((resolve) => bridge.listen(0, '127.0.0.1', resolve))
  const connectionFile = join(folder, 'connection.json')
  const descriptor = { version: 1, port: bridge.address().port, token: secret, pid: process.pid }
  await writeFile(connectionFile, JSON.stringify(descriptor))
  const client = new Client({ name: 'kaicut-protocol-test', version: '1.0.0' })
  const transport = new StdioClientTransport({
    command: process.execPath,
    args: [entry, '--connection-file', connectionFile],
    stderr: 'pipe',
    maxBufferSize: 17 * 1024 * 1024,
  })
  let diagnostics = ''
  transport.stderr.on('data', (chunk) => { diagnostics += chunk.toString() })
  t.after(async () => {
    await client.close()
    for (const socket of sockets) socket.destroy()
    await new Promise((resolve) => bridge.close(resolve))
    await rm(folder, { recursive: true, force: true })
    assert.equal(diagnostics.includes(secret), false, 'credentials must never appear in stderr')
  })
  await client.connect(transport)
  return { client, connectionFile, calls, descriptor }
}

test('MCP handshake and discovery expose bounded inspection and revision checked edits', async (t) => {
  const { client, calls } = await fixture(t)
  await client.ping()
  const { tools } = await client.listTools()
  const names = tools.map((tool) => tool.name)
  for (const name of ['get_state', 'ui_inspect', 'media_probe', 'media_check_links', 'export_job_list', 'export_job_status', 'timeline_edit', 'timeline_undo', 'timeline_redo', 'ui_seek', 'ui_select', 'ui_play', 'ui_pause', 'project_open_path', 'project_import_paths', 'project_save_as', 'export_preflight', 'export_prepare', 'export_job_start', 'export_job_cancel']) {
    assert.ok(names.includes(name), `${name} must be discoverable`)
  }
  assert.equal(names.length, 20)
  assert.equal(names.includes('export_start'), false)
  assert.equal(names.includes('project_save'), false)
  assert.equal(calls.length, 0, 'MCP discovery must not connect to or start the app')
  assert.equal(tools.find((tool) => tool.name === 'get_state').annotations.readOnlyHint, true)
  const edit = tools.find((tool) => tool.name === 'timeline_edit')
  assert.equal(edit.annotations.destructiveHint, true)
  assert.deepEqual(edit.inputSchema.required, ['projectId', 'revision', 'edit'])
  assert.equal(edit.inputSchema.additionalProperties, false)
})

test('export tools forward revision-bound preparation and immutable job actions independently', async (t) => {
  const { client, calls } = await fixture(t, (request) => ({ ok: true, result: { command: request.command, accepted: true } }))
  const context = { projectId: 'project-1', revision: 4, outputPath: 'E:\\验收 素材\\成片.mp4' }
  const cases = [
    ['export_preflight', context], ['export_prepare', context],
    ['export_job_start', { jobId: 'job-1234-abcd' }], ['export_job_cancel', { jobId: 'job-1234-abcd' }],
  ]
  for (const [name, args] of cases) {
    const result = await client.callTool({ name, arguments: args })
    assert.notEqual(result.isError, true)
    assert.deepEqual(calls.at(-1), { token: secret, command: name, args })
    assert.equal(JSON.parse(result.content[0].text).accepted, true)
  }
  const { tools } = await client.listTools()
  assert.equal(tools.find((tool) => tool.name === 'export_preflight').annotations.readOnlyHint, true)
  for (const name of ['export_prepare', 'export_job_start', 'export_job_cancel']) assert.equal(tools.find((tool) => tool.name === name).annotations.readOnlyHint, false)
  assert.equal(tools.find((tool) => tool.name === 'export_job_cancel').annotations.destructiveHint, true)
})

test('export tools reject unsafe paths, forged preflight and extra context on fixed jobs', async (t) => {
  const { client, calls } = await fixture(t)
  const context = { projectId: 'p', revision: 4, outputPath: 'E:\\成片.mp4' }
  const cases = [
    ['export_preflight', { ...context, outputPath: 'relative.mp4' }],
    ['export_preflight', { ...context, outputPath: 'https://remote/file.mp4' }],
    ['export_prepare', { ...context, outputPath: 'E:\\成片.mov' }],
    ['export_prepare', { ...context, preflight: { ready: true } }],
    ['export_prepare', { ...context, overwrite: true }],
    ['export_prepare', { outputPath: 'E:\\成片.mp4', projectId: 'p' }],
    ['export_job_start', { jobId: 'job-123', revision: 4 }],
    ['export_job_start', { jobId: 'job-123', outputPath: 'E:\\other.mp4' }],
    ['export_job_cancel', { jobId: 'job-123', projectId: 'p' }],
  ]
  for (const jobId of ['', 'Job-123', '../job', 'job/x', 'j'.repeat(81)]) {
    for (const name of ['export_job_start', 'export_job_cancel', 'export_job_status']) cases.push([name, { jobId }])
  }
  for (const [name, args] of cases) assert.equal((await client.callTool({ name, arguments: args })).isError, true, name)
  assert.equal(calls.length, 0)
})

test('native stale export context and unknown or running job errors remain tool errors', async (t) => {
  const messages = { export_preflight: '项目 revision 已过期', export_prepare: '项目 revision 已过期', export_job_start: '导出任务不存在', export_job_cancel: '任务已经完成' }
  const { client, calls } = await fixture(t, (request) => ({ ok: false, error: messages[request.command] }))
  for (const [name, message] of Object.entries(messages)) {
    const args = name === 'export_prepare' || name === 'export_preflight' ? { projectId: 'p', revision: 1, outputPath: 'E:\\成片.mp4' } : { jobId: 'job-123' }
    const result = await client.callTool({ name, arguments: args })
    assert.equal(result.isError, true)
    assert.equal(result.content[0].text, message)
  }
  assert.equal(calls.length, 4)
})

test('export preflight and preparation allow native checks longer than the ordinary five second deadline', async (t) => {
  const timers = []
  t.after(() => { timers.forEach(clearTimeout) })
  const { client } = await fixture(t, (_request, socket) => {
    timers.push(setTimeout(() => socket.end(`${JSON.stringify({ ok: true, result: { ready: true } })}\n`), 5300))
  })
  const results = await Promise.all(['export_preflight', 'export_prepare'].map((name) => client.callTool({ name, arguments: { projectId: 'p', revision: 1, outputPath: 'E:\\成片.mp4' } }, undefined, { timeout: 8000 })))
  for (const result of results) {
    assert.notEqual(result.isError, true)
    assert.equal(JSON.parse(result.content[0].text).ready, true)
  }
})

test('project file tools forward explicit paths and revision through the authenticated bridge', async (t) => {
  const { client, calls } = await fixture(t, (request) => ({ ok: true, result: { revision: request.args.revision + 1 } }))
  const context = { projectId: 'project-1', revision: 4 }
  const cases = [
    ['project_open_path', { ...context, path: 'E:\\验收 素材\\剪辑.kaicut' }],
    ['project_import_paths', { ...context, paths: ['E:\\验收 素材\\视频;[literal].mp4', 'E:\\验收 素材\\音频.wav'] }],
    ['project_save_as', { ...context, path: 'E:\\验收 素材\\新项目.kaicut' }],
  ]
  for (const [name, args] of cases) {
    const result = await client.callTool({ name, arguments: args })
    assert.notEqual(result.isError, true)
    assert.deepEqual(calls.at(-1), { token: secret, command: name, args })
  }
  const { tools } = await client.listTools()
  for (const name of cases.map(([name]) => name)) assert.equal(tools.find((tool) => tool.name === name).annotations.readOnlyHint, false)
})

test('project file tools reject relative paths, wrong project extension and uncontrolled options', async (t) => {
  const { client, calls } = await fixture(t)
  const context = { projectId: 'project-1', revision: 4 }
  const cases = [
    ['project_open_path', { ...context, path: 'relative.kaicut' }],
    ['project_open_path', { ...context, path: 'E:\\sample.mp4' }],
    ['project_save_as', { ...context, path: 'E:\\sample.kaicut', overwrite: true }],
    ['project_save_as', { ...context, path: 'https://remote/project.kaicut' }],
    ['project_import_paths', { ...context, paths: [] }],
    ['project_import_paths', { ...context, paths: ['relative.mp4'] }],
    ['project_import_paths', { ...context, paths: Array.from({ length: 101 }, (_, i) => `E:\\${i}.mp4`) }],
    ['project_import_paths', { projectId: 'p', paths: ['E:\\valid.mp4'] }],
  ]
  for (const [name, args] of cases) assert.equal((await client.callTool({ name, arguments: args })).isError, true)
  assert.equal(calls.length, 0)
})

test('inspection uses the authenticated loopback bridge and preserves UTF-8 results', async (t) => {
  const { client, calls } = await fixture(t, (request, socket) => {
    assert.equal(request.token, secret)
    assert.equal(request.command, 'get_state')
    const data = Buffer.from(JSON.stringify({ ok: true, result: { projectId: 'project-1', revision: 3, name: '剪辑验收' } }) + '\n')
    const offset = data.indexOf(Buffer.from('剪')) + 1
    socket.write(data.subarray(0, offset))
    setImmediate(() => socket.end(data.subarray(offset)))
  })
  const result = await client.callTool({ name: 'get_state', arguments: {} })
  assert.notEqual(result.isError, true)
  assert.deepEqual(JSON.parse(result.content[0].text), { projectId: 'project-1', revision: 3, name: '剪辑验收' })
  assert.equal(calls.length, 1)
})

test('editing forwards project/revision and stale native rejection as a tool error', async (t) => {
  const { client, calls } = await fixture(t, (request) => {
    if (request.args.revision !== 3) return { ok: false, error: '项目 revision 已过期，请重新读取。' }
    return { ok: true, result: { revision: 4, canUndo: true } }
  })
  const args = { projectId: 'project-1', revision: 3, edit: { type: 'split', clipId: 'clip-1', frame: 48 } }
  const result = await client.callTool({ name: 'timeline_edit', arguments: args })
  assert.equal(JSON.parse(result.content[0].text).revision, 4)
  assert.deepEqual(calls[0].args, args)
  assert.equal(calls[0].command, 'timeline_edit')
  const stale = await client.callTool({ name: 'timeline_undo', arguments: { projectId: 'project-1', revision: 2 } })
  assert.equal(stale.isError, true)
  assert.match(stale.content[0].text, /revision/)
})

test('invalid and unknown tools/arguments never reach the native bridge', async (t) => {
  const { client, calls } = await fixture(t)
  const cases = [
    ['project_save', {}],
    ['get_state', { token: 'forged' }],
    ['timeline_edit', { projectId: 'p', revision: 0, edit: { type: 'shell', command: 'anything' } }],
    ['timeline_edit', { projectId: 'p', revision: 0, edit: { type: 'split', clipId: 'c', frame: -1 } }],
    ['timeline_edit', { projectId: 'p', revision: 0, edit: { type: 'delete', clipIds: [] } }],
    ['timeline_edit', { projectId: 'p', revision: 0, edit: { type: 'trackVolume', trackId: 'a', volume: 2 } }],
    ['ui_seek', { projectId: 'p', revision: 0.1, frame: 2 }],
    ['ui_seek', { projectId: 'p', frame: 2 }],
    ['ui_select', { projectId: 'p', revision: 0, clipIds: ['x', 'x'] }],
    ['export_job_status', { jobId: '' }],
  ]
  for (const [name, args] of cases) {
    const result = await client.callTool({ name, arguments: args })
    assert.equal(result.isError, true, `${name} must reject ${JSON.stringify(args)}`)
  }
  assert.equal(calls.length, 0)
})

test('empty selection is allowed and all UI actions require current revision', async (t) => {
  const { client, calls } = await fixture(t, (request) => ({ ok: true, result: { applied: request.command } }))
  const args = { projectId: 'p', revision: 5, clipIds: [] }
  assert.notEqual((await client.callTool({ name: 'ui_select', arguments: args })).isError, true)
  assert.deepEqual(calls[0].args, args)
  assert.equal((await client.callTool({ name: 'ui_play', arguments: {} })).isError, true)
  assert.equal(calls.length, 1)
})

test('connection descriptors reject unsupported versions, remote hosts and invalid ports', async (t) => {
  const { client, calls, descriptor, connectionFile } = await fixture(t)
  for (const override of [{ version: 2 }, { host: 'example.com' }, { port: 1.5 }, { port: 65536 }, { port: 0 }, { token: '' }]) {
    await writeFile(connectionFile, JSON.stringify({ ...descriptor, ...override }))
    const result = await client.callTool({ name: 'get_state', arguments: {} })
    assert.equal(result.isError, true)
    assert.equal(result.content[0].text.includes(secret), false)
  }
  assert.equal(calls.length, 0)
})

test('missing and malformed connection files return useful errors without file contents', async (t) => {
  const { client, connectionFile } = await fixture(t)
  await writeFile(connectionFile, `{"token":"${secret}`)
  let result = await client.callTool({ name: 'get_state', arguments: {} })
  assert.equal(result.isError, true)
  assert.equal(result.content[0].text.includes(secret), false)
  await rm(connectionFile)
  result = await client.callTool({ name: 'get_state', arguments: {} })
  assert.equal(result.isError, true)
  assert.match(result.content[0].text, /--mcp/)
})

test('malformed native responses and credential echoes cannot leak the token', async (t) => {
  let count = 0
  const { client } = await fixture(t, (_request, socket) => {
    if (count++ === 0) socket.end(`not json ${secret}\n`)
    else return { ok: false, error: `credential ${secret} rejected` }
  })
  for (let i = 0; i < 2; i += 1) {
    const result = await client.callTool({ name: 'get_state', arguments: {} })
    assert.equal(result.isError, true)
    assert.equal(result.content[0].text.includes(secret), false)
  }
})

test('oversized native responses are rejected instead of buffering without a limit', async (t) => {
  const { client } = await fixture(t, (_request, socket) => {
    socket.on('error', () => {})
    socket.end(' '.repeat(16 * 1024 * 1024 + 1))
  })
  const result = await client.callTool({ name: 'get_state', arguments: {} })
  assert.equal(result.isError, true)
  assert.match(result.content[0].text, /limit|large/i)
})
