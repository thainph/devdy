// Isolation test for the RESUMED-thread path (the real remote-control case).
// Phase 1: start a fresh thread, run a trivial turn, capture its threadId.
// Phase 2: spawn a NEW sidecar that RESUMES that threadId, ask it to write a
// marker, DENY the approval, and assert the marker was NOT created.
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import { existsSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'

const here = dirname(fileURLToPath(import.meta.url))
const CWD = process.env.TEST_CWD || process.cwd()
const marker = join(tmpdir(), `RC_DENY_RESUME_${process.pid}`)
try { rmSync(marker) } catch {}

function baseEnv(mode, extra) {
  const e = { ...process.env }
  delete e.DEVDY_RESUME_SESSION
  return { ...e, DEVDY_PERMISSION_MODE: mode || 'default', ...extra }
}
// Phase 1 can create the thread under a DIFFERENT policy (e.g. the legacy
// on-request default) to prove that resuming under `default` (untrusted) and
// overriding via turn/start still blocks — the real live scenario.
const PHASE1_MODE = process.env.PHASE1_MODE || 'default'

function spawnSidecar(env) {
  return spawn('node', [join(here, 'index.mjs')], {
    cwd: CWD, stdio: ['pipe', 'pipe', 'pipe'], env,
  })
}

function readLines(child, onMsg) {
  let buf = ''
  child.stdout.on('data', (c) => {
    buf += c.toString()
    let nl
    while ((nl = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, nl).trim(); buf = buf.slice(nl + 1)
      if (!line) continue
      let m; try { m = JSON.parse(line) } catch { continue }
      onMsg(m)
    }
  })
  child.stderr.on('data', (c) => process.stderr.write(c))
}

// ── Phase 1: establish a thread ────────────────────────────────────────────
function phase1() {
  return new Promise((resolve, reject) => {
    const child = spawnSidecar(baseEnv(PHASE1_MODE))
    let threadId = null
    let done = false
    const finish = () => { if (done) return; done = true; try { child.kill() } catch {}; resolve(threadId) }
    readLines(child, (m) => {
      if (m.type === 'system' && m.subtype === 'init' && m.session_id) threadId = m.session_id
      if (m.type === '_devdy_error') console.error('[p1 error]', m.error)
      if (m.type === 'result') setTimeout(finish, 300)
    })
    child.on('exit', finish)
    child.stdin.write(JSON.stringify({ type: 'prompt', text: 'Reply with exactly: THREAD_READY. Do not use any tool.' }) + '\n')
    setTimeout(() => reject(new Error('phase1 timeout')), 45_000)
  })
}

// ── Phase 2: resume the thread, deny a marker write ────────────────────────
function phase2(threadId) {
  return new Promise((resolve) => {
    const child = spawnSidecar(baseEnv('default', { DEVDY_RESUME_SESSION: threadId }))
    let sawPermission = false, sentDeny = false, done = false
    const finish = () => {
      if (done) return; done = true; try { child.kill() } catch {}
      const commandRan = existsSync(marker)
      try { rmSync(marker) } catch {}
      resolve({ threadId, sawPermission, sentDeny, commandRan, pass: sawPermission && sentDeny && !commandRan })
    }
    readLines(child, (m) => {
      if (m.type === '_devdy_permission_request' && !sawPermission) {
        sawPermission = true; sentDeny = true
        child.stdin.write(JSON.stringify({ type: 'permission_response', requestId: m.requestId, decision: 'deny' }) + '\n')
      }
      if (m.type === '_devdy_error') console.error('[p2 error]', m.error)
      if (m.type === 'result') setTimeout(finish, 500)
    })
    child.on('exit', finish)
    child.stdin.write(JSON.stringify({ type: 'prompt', text: `Call the shell tool exactly once to run: touch ${marker}\nDo not use any other tool. Do not explain.` }) + '\n')
    setTimeout(finish, 45_000)
  })
}

const threadId = await phase1()
if (!threadId) { console.log(JSON.stringify({ error: 'no threadId from phase1', pass: false })); process.exit(1) }
const result = await phase2(threadId)
console.log(JSON.stringify(result))
process.exit(result.pass ? 0 : 1)
