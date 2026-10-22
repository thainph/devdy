// Tool-less session (DEVDY_CODEX_NO_TOOLS=1): when the configured MCP servers
// cannot be listed (`codex mcp list` fails, prints garbage or not a list), the
// sidecar must refuse to start — an error, and codex app-server never runs.
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import { mkdtempSync, existsSync, readFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import assert from 'node:assert/strict'

const here = dirname(fileURLToPath(import.meta.url))
const fixture = join(here, 'test-fixture-app-server.mjs')

function run(mode) {
  const work = mkdtempSync(join(tmpdir(), 'devdy-codex-failclosed-'))
  const log = join(work, 'fixture.jsonl')
  const env = { ...process.env, DEVDY_CODEX_NO_TOOLS: '1', DEVDY_CODEX_PATH: fixture, DEVDY_FIXTURE_LOG: log, DEVDY_FIXTURE_MCP_LIST: mode }
  delete env.DEVDY_RESUME_SESSION
  return new Promise((resolve) => {
    const child = spawn('node', [join(here, 'index.mjs')], { cwd: work, stdio: ['pipe', 'pipe', 'pipe'], env })
    let out = ''
    child.stdout.on('data', (c) => (out += c))
    const timer = setTimeout(() => child.kill(), 10_000)
    child.on('exit', (code) => {
      clearTimeout(timer)
      const messages = out.split('\n').filter(Boolean).map((l) => { try { return JSON.parse(l) } catch { return null } }).filter(Boolean)
      const appServerRan = existsSync(log) && readFileSync(log, 'utf8').includes('app-server')
      resolve({ code, messages, appServerRan })
    })
    child.stdin.write(JSON.stringify({ type: 'prompt', text: 'go' }) + '\n')
  })
}

for (const mode of ['fail', 'garbage', 'object']) {
  const r = await run(mode)
  const err = r.messages.find((m) => m.type === '_devdy_error')
  assert.ok(err && /Refusing to start a tool-less codex session/.test(err.error), `${mode}: no refusal error: ${JSON.stringify(r.messages)}`)
  assert.equal(r.code, 1, `${mode}: exit code`)
  assert.equal(r.appServerRan, false, `${mode}: codex app-server was started`)
  assert.equal(r.messages.filter((m) => m.type === 'result').length, 0, `${mode}: produced a result`)
}

console.log('ok   tool-less session refuses to start when MCP servers cannot be listed')
