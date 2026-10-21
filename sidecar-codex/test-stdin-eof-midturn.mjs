// Regression test: the broker closing stdin while a Codex turn is in flight must
// not kill the turn. The open approval is declined + reported cancelled, the turn
// runs to completion, and only then does the session close (cleanly, code 0).
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import { existsSync, mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'

const here = dirname(fileURLToPath(import.meta.url))
const work = mkdtempSync(join(tmpdir(), 'devdy-codex-eof-'))
const marker = join(work, 'COMMAND_RAN')
const env = {
  ...process.env,
  DEVDY_PERMISSION_MODE: 'default',
  DEVDY_CODEX_PATH: join(here, 'test-fixture-app-server.mjs'),
  DEVDY_DENY_TEST_MARKER: marker,
}
delete env.DEVDY_RESUME_SESSION
const child = spawn('node', [join(here, 'index.mjs')], { cwd: work, stdio: ['pipe', 'pipe', 'pipe'], env })

const order = [] // message types in arrival order
let requestId = null
let cancelledId = null
let closed = null
let buf = ''
const timer = setTimeout(() => { child.kill('SIGKILL'); report('timeout') }, 15_000)

child.stdout.on('data', (chunk) => {
  buf += chunk.toString()
  let nl
  while ((nl = buf.indexOf('\n')) >= 0) {
    const line = buf.slice(0, nl).trim()
    buf = buf.slice(nl + 1)
    if (!line) continue
    let msg
    try { msg = JSON.parse(line) } catch { continue }
    order.push(msg.type)
    if (msg.type === '_devdy_ready') child.stdin.write(JSON.stringify({ type: 'prompt', text: 'go' }) + '\n')
    if (msg.type === '_devdy_permission_request') {
      requestId = msg.requestId
      child.stdin.end() // never answer: the input channel closes mid-turn
    }
    if (msg.type === '_devdy_permission_cancelled') cancelledId = msg.requestId
    if (msg.type === '_devdy_closed') closed = msg
  }
})
child.on('exit', (code) => { clearTimeout(timer); report(code) })

function report(exitCode) {
  const resultAt = order.indexOf('result')
  const closedAt = order.indexOf('_devdy_closed')
  const checks = {
    waiterCancelled: requestId !== null && cancelledId === requestId,
    turnCompleted: resultAt !== -1,
    closedOnlyAfterCompletion: closedAt > resultAt && resultAt !== -1,
    cleanClose: closed?.code === 0 && closed?.signal === null && exitCode === 0,
    commandNotRun: !existsSync(marker),
  }
  const pass = Object.values(checks).every(Boolean)
  console.log(JSON.stringify({ ...checks, order, pass }))
  process.exit(pass ? 0 : 1)
}
