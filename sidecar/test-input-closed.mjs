#!/usr/bin/env node
// Regression test: a permission request open when the broker closes our stdin
// must not hang the session.
//
// Once stdin is closed no permission answer can ever arrive. The SDK still keeps
// the CLI running until its turn ends, so the sidecar must settle the open
// request itself (deny + `_devdy_permission_cancelled`), let the turn finish,
// report `_devdy_closed` and EXIT — instead of waiting forever.
//
// Run: node test-input-closed.mjs   (from the sidecar/ dir; makes one real turn)

import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import fs from 'node:fs'
import os from 'node:os'

const here = dirname(fileURLToPath(import.meta.url))
const testDir = join(os.tmpdir(), 'devdy-sidecar-input-closed-test')
fs.rmSync(testDir, { recursive: true, force: true })
fs.mkdirSync(testDir, { recursive: true })

const env = { ...process.env }
delete env.ANTHROPIC_API_KEY
delete env.ANTHROPIC_AUTH_TOKEN

const child = spawn('node', [join(here, 'index.mjs')], {
  cwd: testDir,
  env,
  stdio: ['pipe', 'pipe', 'inherit'],
})

let requested = null
let cancelled = false
let sawResult = false
let closed = false
let buffer = ''

const HARD_TIMEOUT = setTimeout(() => {
  console.error('\n[TIMEOUT] sidecar did not finish within 150s — the session hung')
  child.kill('SIGKILL')
  report(false)
}, 150_000)

function report(exited) {
  clearTimeout(HARD_TIMEOUT)
  console.log('\n================ INPUT-CLOSED TEST RESULT ================')
  console.log('permission requested :', requested ? `YES ✅ (${requested})` : 'NO ❌')
  console.log('request cancelled    :', cancelled ? 'YES ✅' : 'NO ❌')
  console.log('turn result          :', sawResult ? 'YES ✅' : 'NO ❌')
  console.log('_devdy_closed        :', closed ? 'YES ✅' : 'NO ❌')
  console.log('process exited       :', exited ? 'YES ✅' : 'NO ❌')
  console.log('==========================================================')
  const pass = !!requested && cancelled && sawResult && closed && exited
  console.log(pass ? 'PASS ✅' : 'FAIL ❌')
  process.exit(pass ? 0 : 1)
}

child.stdout.on('data', (chunk) => {
  buffer += chunk.toString()
  let idx
  while ((idx = buffer.indexOf('\n')) >= 0) {
    const line = buffer.slice(0, idx)
    buffer = buffer.slice(idx + 1)
    if (!line.trim()) continue
    let msg
    try { msg = JSON.parse(line) } catch { continue }
    handle(msg)
  }
})

function handle(msg) {
  switch (msg.type) {
    case '_devdy_ready':
      child.stdin.write(JSON.stringify({
        type: 'prompt',
        text: 'Create a file named out.txt containing the word hello in the current directory. ' +
          'If you are not allowed to, just say so and stop. Do not retry.',
        options: { cwd: testDir, permissionMode: 'default' },
      }) + '\n')
      break
    case '_devdy_permission_request':
      // Don't answer: close the input channel instead (what the broker does at
      // turn end). Without the fix the sidecar waits on this request forever.
      requested = msg.tool_name
      console.log(`[permission] ${msg.tool_name} → closing stdin instead of answering`)
      child.stdin.end()
      break
    case '_devdy_permission_cancelled':
      cancelled = true
      console.log(`[cancelled] ${msg.text ?? ''}`)
      break
    case '_devdy_closed':
      closed = true
      console.log('[closed]')
      break
    case 'result':
      sawResult = true
      console.log(`[result] subtype=${msg.subtype}`)
      break
    default:
      break
  }
}

child.on('exit', () => report(true))
