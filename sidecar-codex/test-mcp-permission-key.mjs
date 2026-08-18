// Regression test: Codex MCP permission prompts must expose a stable per-tool
// key so Devdy's "allow always" / "deny always" store can match them.
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'

const here = dirname(fileURLToPath(import.meta.url))
const work = mkdtempSync(join(tmpdir(), 'devdy-codex-mcp-key-'))
const fixture = join(here, 'test-fixture-app-server.mjs')

const childEnv = { ...process.env }
delete childEnv.DEVDY_RESUME_SESSION

const child = spawn('node', [join(here, 'index.mjs')], {
  cwd: work,
  stdio: ['pipe', 'pipe', 'pipe'],
  env: {
    ...childEnv,
    DEVDY_PERMISSION_MODE: 'default',
    DEVDY_CODEX_PATH: fixture,
    DEVDY_MCP_PERMISSION_KEY_TEST: '1',
  },
})

let buffer = ''
let sawPermission = false
let sawStableToolName = false
let sawAcceptedResult = false
let finished = false

function finish(code = 0) {
  if (finished) return
  finished = true
  try { child.kill() } catch {}
  const pass = sawPermission && sawStableToolName && sawAcceptedResult
  console.log(JSON.stringify({ sawPermission, sawStableToolName, sawAcceptedResult, pass }))
  process.exit(pass ? 0 : code || 1)
}

child.stdout.on('data', (chunk) => {
  buffer += chunk.toString()
  let newline
  while ((newline = buffer.indexOf('\n')) >= 0) {
    const line = buffer.slice(0, newline).trim()
    buffer = buffer.slice(newline + 1)
    if (!line) continue
    let message
    try { message = JSON.parse(line) } catch { continue }

    if (message.type === '_devdy_permission_request') {
      sawPermission = true
      sawStableToolName = message.tool_name === 'mcp__google_drive__list_files'
      child.stdin.write(JSON.stringify({
        type: 'permission_response',
        requestId: message.requestId,
        decision: 'allow',
      }) + '\n')
    }
    if (message.type === 'user') {
      for (const item of message.message?.content ?? []) {
        if (item.type === 'tool_result' && item.is_error === false) sawAcceptedResult = true
      }
    }
    if (message.type === 'result') finish(0)
    if (message.type === '_devdy_error') {
      console.error(message.error)
      finish(1)
    }
  }
})

child.stderr.on('data', (chunk) => process.stderr.write(chunk))
child.on('exit', (code) => finish(code ?? 1))
child.stdin.write(JSON.stringify({
  type: 'prompt',
  text: 'Trigger the fixture MCP permission request.',
}) + '\n')

setTimeout(() => finish(1), 10_000)
