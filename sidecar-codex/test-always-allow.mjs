// Regression test for "allow always" under codex.
//
// Two things used to make Devdy re-ask forever where Claude asks once:
//   1. An MCP elicitation has no `toolName` in the codex app-server v2 protocol,
//      so the permission key collapsed to the bare string `MCP` — which the
//      prompt deliberately refuses to remember. It must be `mcp__<server>`.
//   2. An "always" allow was answered to codex as a plain one-shot `accept`, so
//      codex kept asking for every matching command. It must be
//      `acceptForSession`, which opts into codex's own session approval cache.
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import { mkdtempSync, readFileSync, existsSync } from 'node:fs'
import { tmpdir } from 'node:os'

const here = dirname(fileURLToPath(import.meta.url))
const fixture = join(here, 'test-fixture-app-server.mjs')

function runCase({ env, onPermission, done }) {
  return new Promise((resolve) => {
    const work = mkdtempSync(join(tmpdir(), 'devdy-codex-always-'))
    const childEnv = { ...process.env }
    delete childEnv.DEVDY_RESUME_SESSION
    const child = spawn('node', [join(here, 'index.mjs')], {
      cwd: work,
      stdio: ['pipe', 'pipe', 'pipe'],
      env: { ...childEnv, DEVDY_PERMISSION_MODE: 'default', DEVDY_CODEX_PATH: fixture, ...env },
    })

    let buffer = ''
    let settled = false
    const finish = () => {
      if (settled) return
      settled = true
      try { child.kill() } catch {}
      resolve(done())
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
        if (message.type === '_devdy_permission_request') onPermission(message, child)
        if (message.type === 'result' || message.type === '_devdy_error') {
          // Give the fixture a beat to flush its side effects before asserting.
          setTimeout(finish, 150)
        }
      }
    })
    child.on('exit', finish)
    child.stdin.write(JSON.stringify({ type: 'prompt', text: 'go' }) + '\n')
    setTimeout(finish, 10_000)
  })
}

// ── Case 1: MCP elicitation WITHOUT a toolName → per-server key ────────────
let mcpToolName = null
await runCase({
  env: { DEVDY_MCP_PERMISSION_KEY_TEST: '1', DEVDY_MCP_OMIT_TOOLNAME: '1' },
  onPermission: (msg, child) => {
    mcpToolName = msg.tool_name
    child.stdin.write(JSON.stringify({
      type: 'permission_response', requestId: msg.requestId, decision: 'allow',
    }) + '\n')
  },
  done: () => null,
})

// ── Case 2: an "always" allow reaches codex as `acceptForSession` ──────────
const decisionFile = join(mkdtempSync(join(tmpdir(), 'devdy-codex-decision-')), 'decision')
await runCase({
  env: { DEVDY_DENY_TEST_MARKER: join(tmpdir(), 'devdy-always-marker'), DEVDY_DECISION_OUT: decisionFile },
  onPermission: (msg, child) => {
    child.stdin.write(JSON.stringify({
      type: 'permission_response', requestId: msg.requestId, decision: 'allow', remember: true,
    }) + '\n')
  },
  done: () => null,
})
const sessionDecision = existsSync(decisionFile) ? readFileSync(decisionFile, 'utf8') : null

const mcpKeyIsPerServer = mcpToolName === 'mcp__google_drive'
const allowAlwaysIsSessionScoped = sessionDecision === 'acceptForSession'
const pass = mcpKeyIsPerServer && allowAlwaysIsSessionScoped
console.log(JSON.stringify({ mcpToolName, sessionDecision, mcpKeyIsPerServer, allowAlwaysIsSessionScoped, pass }))
process.exit(pass ? 0 : 1)
