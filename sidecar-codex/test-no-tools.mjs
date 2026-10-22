// Tool-less session (DEVDY_CODEX_NO_TOOLS=1, the translation engine): codex
// must start with exec/app/plugin features off, every MCP server disabled, a
// read-only sandbox, and any approval request declined without a prompt — even
// when the broker passes MCP servers or a looser policy override.
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import { mkdtempSync, readFileSync, existsSync } from 'node:fs'
import { tmpdir } from 'node:os'
import assert from 'node:assert/strict'

const here = dirname(fileURLToPath(import.meta.url))
const fixture = join(here, 'test-fixture-app-server.mjs')

const work = mkdtempSync(join(tmpdir(), 'devdy-codex-notools-'))
const marker = join(work, 'executed')
const log = join(work, 'fixture.jsonl')
const childEnv = { ...process.env }
delete childEnv.DEVDY_RESUME_SESSION

const messages = await new Promise((resolve) => {
  const child = spawn('node', [join(here, 'index.mjs')], {
    cwd: work,
    stdio: ['pipe', 'pipe', 'pipe'],
    env: {
      ...childEnv,
      DEVDY_CODEX_NO_TOOLS: '1',
      DEVDY_PERMISSION_MODE: 'bypassPermissions',
      DEVDY_CODEX_APPROVAL: 'never',
      DEVDY_CODEX_SANDBOX: 'danger-full-access',
      DEVDY_CODEX_MCP: JSON.stringify({ broker: { type: 'stdio', command: 'evil-mcp' } }),
      DEVDY_CODEX_PATH: fixture,
      DEVDY_DENY_TEST_MARKER: marker,
      DEVDY_FIXTURE_LOG: log,
    },
  })
  const seen = []
  let buffer = ''
  let settled = false
  const finish = () => {
    if (settled) return
    settled = true
    try { child.kill() } catch {}
    resolve(seen)
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
      seen.push(message)
      if (message.type === 'result' || message.type === '_devdy_error') setTimeout(finish, 150)
    }
  })
  child.on('exit', finish)
  child.stdin.write(JSON.stringify({ type: 'prompt', text: 'go' }) + '\n')
  setTimeout(finish, 10_000)
})

const entries = readFileSync(log, 'utf8').trim().split('\n').map((l) => JSON.parse(l))
const argv = entries.find((e) => e.argv).argv
const threadStart = entries.find((e) => e.method === 'thread/start').params
const turnStart = entries.find((e) => e.method === 'turn/start').params

assert.ok(argv.includes('features.shell_tool=false'), `shell tool not disabled: ${argv}`)
assert.ok(argv.includes('features.unified_exec=false'))
assert.ok(argv.includes('web_search="disabled"'))
assert.ok(argv.includes('mcp_servers.user_server.enabled=false'), 'user MCP server not disabled')
assert.ok(!argv.some((a) => a.includes('evil-mcp') || a.startsWith('mcp_servers.broker')), 'broker MCP leaked')
assert.equal(argv[argv.length - 1], 'app-server')
assert.equal(threadStart.approvalPolicy, 'untrusted')
assert.equal(threadStart.sandbox, 'read-only')
assert.equal(turnStart.approvalPolicy, 'untrusted')
assert.deepEqual(turnStart.sandboxPolicy, { type: 'readOnly' })
assert.equal(messages.filter((m) => m.type === '_devdy_permission_request').length, 0, 'raised a permission prompt')
assert.ok(messages.some((m) => m.type === '_devdy_log' && /no tool access/.test(m.text)))
assert.equal(existsSync(marker), false, 'command approval was not declined')

console.log('ok   tool-less session: no exec/MCP, read-only, approvals declined')
