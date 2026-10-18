import { open } from 'node:fs/promises'
import { connect } from 'node:net'
import { homedir } from 'node:os'
import { join } from 'node:path'

export const MAX_REQUEST_BYTES = 1024 * 1024
const MAX_RESPONSE_BYTES = 16 * 1024 * 1024
const MAX_CONNECTION_BYTES = 16 * 1024

export function connectionFilePath(argv = process.argv.slice(2), env = process.env) {
  if (argv.length && (argv.length !== 2 || argv[0] !== '--connection-file' || !argv[1])) {
    throw new Error('Usage: node mcp/server.mjs [--connection-file <path>]')
  }
  if (argv[1]) return argv[1]
  if (env.KAICUT_MCP_CONNECTION_FILE) return env.KAICUT_MCP_CONNECTION_FILE
  if (process.platform === 'win32') return join(env.LOCALAPPDATA || join(homedir(), 'AppData', 'Local'), 'com.kaicut.desktop', 'mcp', 'connection.json')
  if (process.platform === 'darwin') return join(homedir(), 'Library', 'Application Support', 'com.kaicut.desktop', 'mcp', 'connection.json')
  return join(env.XDG_DATA_HOME || join(homedir(), '.local', 'share'), 'com.kaicut.desktop', 'mcp', 'connection.json')
}

async function readConnection(path) {
  let file
  let text
  try {
    file = await open(path, 'r')
    const buffer = Buffer.alloc(MAX_CONNECTION_BYTES + 1)
    const { bytesRead } = await file.read(buffer, 0, buffer.length, 0)
    if (bytesRead > MAX_CONNECTION_BYTES) throw new Error('connection file too large')
    text = buffer.subarray(0, bytesRead).toString('utf8')
  } catch {
    throw new Error('KaiCut connection file is unavailable. Start KaiCut with --mcp, or set KAICUT_MCP_CONNECTION_FILE to its connection.json.')
  } finally {
    await file?.close()
  }
  let descriptor
  try { descriptor = JSON.parse(text) } catch {
    throw new Error('Invalid KaiCut connection file JSON. Restart KaiCut with --mcp.')
  }
  if (!descriptor || typeof descriptor !== 'object' || Array.isArray(descriptor)
    || descriptor.version !== 1
    || !Number.isInteger(descriptor.port) || descriptor.port < 1 || descriptor.port > 65535
    || !Number.isSafeInteger(descriptor.pid) || descriptor.pid < 1
    || typeof descriptor.token !== 'string' || descriptor.token.length < 32 || descriptor.token.length > 256
    || /[\r\n]/u.test(descriptor.token)
    || (descriptor.host !== undefined && descriptor.host !== '127.0.0.1')) {
    throw new Error('Unsupported or invalid KaiCut connection descriptor. Only version 1 on 127.0.0.1 is supported.')
  }
  return descriptor
}

export async function requestBridge(connectionFile, command, args, { timeoutMs = ['media_probe', 'export_preflight', 'export_prepare'].includes(command) ? 60000 : command.startsWith('project_') ? 30000 : 5000, signal } = {}) {
  const recovery = command.startsWith('export_')
    ? 'Cancelling or timing out this request does not cancel the export job. Check export_job_list/export_job_status before retrying; never blindly retry.'
    : 'An edit may already have applied; re-read state before retrying.'
  if (signal?.aborted) throw new Error(`KaiCut request cancelled. ${recovery}`)
  const descriptor = await readConnection(connectionFile)
  const request = `${JSON.stringify({ token: descriptor.token, command, args })}\n`
  if (Buffer.byteLength(request) > MAX_REQUEST_BYTES) throw new Error('KaiCut request exceeds the 1 MiB size limit.')
  return new Promise((resolve, reject) => {
    const socket = connect({ host: '127.0.0.1', port: descriptor.port })
    const chunks = []
    let bytes = 0
    let settled = false
    const finish = (error, result) => {
      if (settled) return
      settled = true
      clearTimeout(timer)
      signal?.removeEventListener('abort', abort)
      socket.destroy()
      if (error) reject(error)
      else resolve(result)
    }
    const abort = () => finish(new Error(`KaiCut request cancelled. ${recovery}`))
    const timer = setTimeout(() => finish(new Error(`KaiCut request timed out. ${recovery}`)), timeoutMs)
    signal?.addEventListener('abort', abort, { once: true })
    if (signal?.aborted) { abort(); return }
    socket.once('connect', () => socket.write(request))
    socket.on('data', (chunk) => {
      bytes += chunk.length
      if (bytes > MAX_RESPONSE_BYTES) {
        finish(new Error('KaiCut response exceeds the 16 MiB size limit.'))
        return
      }
      const newline = chunk.indexOf(10)
      chunks.push(newline < 0 ? chunk : chunk.subarray(0, newline))
      if (newline < 0) return
      let response
      try { response = JSON.parse(Buffer.concat(chunks).toString('utf8')) } catch {
        finish(new Error('KaiCut returned invalid bridge JSON.'))
        return
      }
      if (response?.ok === true && Object.hasOwn(response, 'result')) {
        // The bridge credential is never part of an MCP result, even if a buggy
        // local peer accidentally includes it in an error or inspection payload.
        const safeJson = JSON.stringify(response.result).split(descriptor.token).join('[redacted]')
        finish(null, JSON.parse(safeJson))
      } else if (response?.ok === false && typeof response.error === 'string') {
        finish(new Error(response.error.split(descriptor.token).join('[redacted]').slice(0, 4096)))
      } else {
        finish(new Error('KaiCut returned an invalid bridge response envelope.'))
      }
    })
    socket.once('error', () => finish(new Error('Cannot connect to KaiCut. Start the app with --mcp and check the connection file.')))
    socket.once('end', () => finish(new Error('KaiCut closed the connection without a complete response.')))
  })
}
