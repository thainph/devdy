// Regression test: how a Codex session ends must reach the broker.
//   1. codex killed by a signal → `_devdy_closed { code: null, signal }` and the
//      sidecar exits non-zero (the broker marks the run failed);
//   2. a normal end (stdin closed → the sidecar itself kills codex) →
//      `_devdy_closed { code: 0, signal: null }` and exit 0 (run done).
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'

const here = dirname(fileURLToPath(import.meta.url))

function run(fixture, onReady) {
  return new Promise((resolve) => {
    const env = { ...process.env, DEVDY_CODEX_PATH: join(here, fixture) }
    delete env.DEVDY_RESUME_SESSION
    const child = spawn('node', [join(here, 'index.mjs')], {
      cwd: mkdtempSync(join(tmpdir(), 'devdy-codex-exit-')),
      stdio: ['pipe', 'pipe', 'pipe'],
      env,
    })
    const timer = setTimeout(() => { child.kill('SIGKILL'); resolve({ closed: null, code: 'timeout' }) }, 15_000)
    let buf = ''
    let closed = null
    child.stdout.on('data', (chunk) => {
      buf += chunk.toString()
      let nl
      while ((nl = buf.indexOf('\n')) >= 0) {
        const line = buf.slice(0, nl).trim()
        buf = buf.slice(nl + 1)
        if (!line) continue
        let msg
        try { msg = JSON.parse(line) } catch { continue }
        if (msg.type === '_devdy_ready') onReady?.(child)
        if (msg.type === '_devdy_closed') closed = msg
      }
    })
    child.on('exit', (code) => { clearTimeout(timer); resolve({ closed, code }) })
  })
}

const crashed = await run('test-fixture-sigterm.mjs')
const crashOk = crashed.closed?.code === null && crashed.closed?.signal === 'SIGTERM' && crashed.code !== 0

const normal = await run('test-fixture-app-server.mjs', (child) => child.stdin.end())
const normalOk = normal.closed?.code === 0 && normal.closed?.signal === null && normal.code === 0

const pass = crashOk && normalOk
console.log(JSON.stringify({ crashed, normal, pass }))
process.exit(pass ? 0 : 1)
