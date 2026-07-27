// Isolation test: does a DELAYED "deny" (cancel) still block a real Codex
// command? Reproduces the live remote-control latency (~10s dialog round-trip).
//
// Uses the REAL `codex app-server` (needs ChatGPT auth + network). The prompt
// asks Codex to write a marker OUTSIDE the workspace so `workspace-write`
// forces an approval request. We wait DELAY_MS after seeing the request before
// sending `deny`, then assert the marker was NOT created.
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import { existsSync, mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'

const here = dirname(fileURLToPath(import.meta.url))
const work = process.env.TEST_CWD || mkdtempSync(join(tmpdir(), 'devdy-codex-denylat-'))
const marker = join(tmpdir(), `RC_DENY_LATENCY_${process.pid}`)
try { rmSync(marker) } catch {}
const DELAY_MS = Number(process.env.DENY_DELAY_MS || '12000')

// Strip ambient Devdy runtime vars (e.g. DEVDY_RESUME_SESSION) so the sidecar
// starts a fresh thread instead of trying to resume the caller's session.
const childEnv = { ...process.env }
delete childEnv.DEVDY_RESUME_SESSION
// Optionally resume a SPECIFIC existing thread (to reproduce the live case of a
// thread created by an older app build).
if (process.env.RESUME_THREAD_ID) childEnv.DEVDY_RESUME_SESSION = process.env.RESUME_THREAD_ID
const child = spawn('node', [join(here, 'index.mjs')], {
  cwd: work,
  stdio: ['pipe', 'pipe', 'pipe'],
  env: { DEVDY_PERMISSION_MODE: 'default', ...childEnv },
})

let buffer = ''
let sawPermission = false
let sentDeny = false
let finished = false

function finish(code) {
  if (finished) return
  finished = true
  try { child.kill() } catch {}
  const commandRan = existsSync(marker)
  try { rmSync(marker) } catch {}
  const pass = sawPermission && sentDeny && !commandRan
  console.log(JSON.stringify({ sawPermission, sentDeny, delayMs: DELAY_MS, commandRan, pass }))
  process.exit(pass ? 0 : (code || 1))
}

child.stdout.on('data', (chunk) => {
  buffer += chunk.toString()
  let nl
  while ((nl = buffer.indexOf('\n')) >= 0) {
    const line = buffer.slice(0, nl).trim()
    buffer = buffer.slice(nl + 1)
    if (!line) continue
    let m
    try { m = JSON.parse(line) } catch { continue }
    if (m.type === '_devdy_permission_request' && !sawPermission) {
      sawPermission = true
      console.error(`[test] permission request seen; waiting ${DELAY_MS}ms before deny`)
      setTimeout(() => {
        sentDeny = true
        console.error('[test] sending deny now')
        child.stdin.write(JSON.stringify({ type: 'permission_response', requestId: m.requestId, decision: 'deny' }) + '\n')
      }, DELAY_MS)
    }
    if (m.type === '_devdy_error') console.error('[sidecar error]', m.error)
    if (m.type === '_devdy_log') console.error('[sidecar log]', m.text)
    if (m.type === 'result') setTimeout(() => finish(0), 500)
  }
})
child.stderr.on('data', (c) => process.stderr.write(c))
child.on('exit', (code) => finish(code ?? 1))
child.stdin.write(JSON.stringify({
  type: 'prompt',
  text: `Call the shell tool exactly once to run: touch ${marker}\nDo not use any other tool. Do not explain.`,
}) + '\n')

setTimeout(() => finish(1), DELAY_MS + 40_000)
