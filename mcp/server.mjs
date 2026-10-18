#!/usr/bin/env node
import { Server } from '@modelcontextprotocol/sdk/server/index.js'
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js'
import { CallToolRequestSchema, ListToolsRequestSchema } from '@modelcontextprotocol/sdk/types.js'
import { AjvJsonSchemaValidator } from '@modelcontextprotocol/sdk/validation/ajv'
import { connectionFilePath, MAX_REQUEST_BYTES, requestBridge } from './bridge.mjs'
import { toolDefinitions } from './tools.mjs'

const errorResult = (message) => ({ isError: true, content: [{ type: 'text', text: message }] })

async function main() {
  const connectionFile = connectionFilePath()
  const server = new Server({ name: 'kaicut', version: '0.1.0' }, {
    capabilities: { tools: {} },
    instructions: 'KaiCut local desktop editing and inspection. Start KaiCut with --mcp before calling tools. Inspect get_state and ui_inspect first. Edits require the latest projectId/revision. Structured UI inspection is not a screenshot and cannot establish pixel-level appearance or playback smoothness. Do not blindly retry timed-out mutations.',
  })
  const validator = new AjvJsonSchemaValidator()
  const validators = new Map(toolDefinitions.map((tool) => [tool.name, validator.getValidator(tool.inputSchema)]))
  server.setRequestHandler(ListToolsRequestSchema, async () => ({ tools: toolDefinitions }))
  server.setRequestHandler(CallToolRequestSchema, async (request, extra) => {
    const validate = validators.get(request.params.name)
    if (!validate) return errorResult('Unknown KaiCut tool. Use tools/list to see supported operations.')
    const args = request.params.arguments ?? {}
    if (!validate(args).valid) return errorResult('Invalid KaiCut tool arguments. Follow its inputSchema; unknown fields, invalid IDs, frames or revisions are rejected.')
    try {
      const result = await requestBridge(connectionFile, request.params.name, args, { signal: extra.signal })
      return { content: [{ type: 'text', text: JSON.stringify(result) }] }
    } catch (error) {
      return errorResult(error instanceof Error ? error.message : 'KaiCut request failed.')
    }
  })
  // stdout belongs exclusively to MCP JSON-RPC. Diagnostics never echo input,
  // descriptors or thrown parser data that may contain the bridge credential.
  server.onerror = () => { process.stderr.write('KaiCut MCP protocol error.\n') }
  const transport = new StdioServerTransport(process.stdin, process.stdout, { maxBufferSize: MAX_REQUEST_BYTES })
  await server.connect(transport)
  process.stdin.once('end', () => { void server.close() })
}

main().catch(() => {
  process.stderr.write('KaiCut MCP server failed to start. Usage: node mcp/server.mjs [--connection-file <path>]\n')
  process.exitCode = 1
})
