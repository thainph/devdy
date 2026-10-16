// Framing reaches Codex the right way:
//  (A) fresh start → `developerInstructions` on thread/start, reported seeded;
//  (B) resume → Codex ignores developerInstructions on thread/resume, so the
//      <session-framing> seed is prepended to the FIRST turn only, reported
//      seeded once, and never leaks into the codex child env.
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import { mkdtempSync, readFileSync, existsSync } from 'node:fs'
import { tmpdir } from 'node:os'

const here = dirname(fileURLToPath(import.meta.url))
const fixture = join(here, 'test-fixture-framing.mjs')

function runSidecar(name, env, prompts) {
  const work = mkdtempSync(join(tmpdir(), `devdy-codex-framing-${name}-`))
  const rpcOut = join(work, 'rpc.ndjson')
  const childEnv = { ...process.env }
  for (const k of ['DEVDY_RESUME_SESSION', 'DEVDY_CODEX_DEVELOPER_INSTRUCTIONS', 'DEVDY_CODEX_SEED_FRAMING', 'DEVDY_CODEX_MCP']) delete childEnv[k]
  return new Promise((resolve) => {
    const child = spawn('node', [join(here, 'index.mjs')], {
      cwd: work,
      stdio: ['pipe', 'pipe', 'pipe'],
      env: { ...childEnv, DEVDY_PERMISSION_MODE: 'default', DEVDY_CODEX_PATH: fixture, DEVDY_FRAMING_RPC_OUT: rpcOut, ...env },
    })
    const ctrl = []
    let buf = ''
    child.stdout.on('data', (d) => {
      buf += d
      let nl
      while ((nl = buf.indexOf('\n')) >= 0) {
        const line = buf.slice(0, nl); buf = buf.slice(nl + 1)
        try { const m = JSON.parse(line); if (m.type?.startsWith('_devdy_')) ctrl.push(m.type) } catch {}
      }
    })
    for (const text of prompts) child.stdin.write(JSON.stringify({ type: 'prompt', text }) + '\n')
    setTimeout(() => {
      try { child.kill() } catch {}
      const rpc = existsSync(rpcOut)
        ? readFileSync(rpcOut, 'utf8').trim().split('\n').filter(Boolean).map((l) => JSON.parse(l))
        : []
      resolve({ rpc, ctrl })
    }, 1500)
  })
}

const seeded = (ctrl) => ctrl.filter((t) => t === '_devdy_framing_seeded').length
const turnTexts = (rpc) => rpc.filter((r) => r.method === 'turn/start').map((r) => r.params.input?.[0]?.text)

const a = await runSidecar('start', { DEVDY_CODEX_DEVELOPER_INSTRUCTIONS: 'FRAMING-A' }, ['hello'])
const aStart = a.rpc.find((r) => r.method === 'thread/start')
const startOk = aStart?.params?.developerInstructions === 'FRAMING-A' &&
  seeded(a.ctrl) === 1 && turnTexts(a.rpc)[0] === 'hello'

const seed = '<session-framing>\nFRAMING-B\n</session-framing>'
const b = await runSidecar('resume', { DEVDY_RESUME_SESSION: 'thr-old', DEVDY_CODEX_SEED_FRAMING: seed }, ['first', 'second'])
const bResume = b.rpc.find((r) => r.method === 'thread/resume')
const texts = turnTexts(b.rpc)
const resumeOk = bResume && !('developerInstructions' in bResume.params) &&
  texts[0] === `${seed}\n\nfirst` && texts[1] === 'second' && seeded(b.ctrl) === 1

const c = await runSidecar('plain', {}, ['plain'])
const plainOk = seeded(c.ctrl) === 0 && turnTexts(c.rpc)[0] === 'plain' &&
  !('developerInstructions' in (c.rpc.find((r) => r.method === 'thread/start')?.params || {}))

const pass = !!(startOk && resumeOk && plainOk)
console.log(JSON.stringify({ startOk, resumeOk, plainOk, texts, pass }))
process.exit(pass ? 0 : 1)
