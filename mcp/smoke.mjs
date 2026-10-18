#!/usr/bin/env node
import { fileURLToPath } from 'node:url'
import { Client } from '@modelcontextprotocol/sdk/client/index.js'
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js'
import { connectionFilePath } from './bridge.mjs'

// This checks the live application through MCP without starting KaiCut,
// moving a pointer, editing a project, playing media, or starting exports.
const client = new Client({ name: 'kaicut-smoke', version: '0.1.0' })
try {
  const transport = new StdioClientTransport({
    command: process.execPath,
    args: [fileURLToPath(new URL('./server.mjs', import.meta.url)), '--connection-file', connectionFilePath()],
    stderr: 'inherit',
    maxBufferSize: 17 * 1024 * 1024,
  })
  await client.connect(transport)
  await client.ping()
  const discovery = await client.listTools()
  const results = {}
  for (const name of ['get_state', 'ui_inspect']) {
    const result = await client.callTool({ name, arguments: {} })
    if (result.isError) throw new Error(result.content[0]?.text || `${name} failed`)
    results[name] = JSON.parse(result.content[0].text)
  }
  const state = results.get_state.session || results.get_state
  const project = state.project || {}
  const ui = results.ui_inspect
  // Only report counts and UI health, not media paths, credentials or project contents.
  console.log(JSON.stringify({
    ok: true,
    server: client.getServerVersion(),
    toolCount: discovery.tools.length,
    project: {
      revision: state.revision,
      dirty: state.dirty,
      trackCount: project.tracks?.length,
      clipCount: project.tracks?.reduce((count, track) => count + track.clips.length, 0),
      mediaCount: project.mediaRefs?.length,
    },
    ui: {
      revision: ui.revision,
      playheadFrame: ui.playheadFrame,
      previewMode: ui.preview?.mode,
      isPlaying: ui.preview?.playing,
      selectedClipCount: ui.selectedClipIds?.length,
      availableFields: Object.keys(ui),
    },
  }, null, 2))
} catch (error) {
  console.error(error instanceof Error ? error.message : 'KaiCut MCP smoke failed.')
  process.exitCode = 1
} finally {
  await client.close()
}
