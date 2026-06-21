#!/usr/bin/env node
// Standalone smoke test for the Devdy Claude sidecar.
//
// Verifies end-to-end WITHOUT Tauri:
//   1. The Agent SDK runs on the current login (subscription via Keychain) —
//      no ANTHROPIC_API_KEY is set in env.
//   2. Streaming SDK messages arrive over the NDJSON bridge.
//   3. canUseTool fires for a mutating tool and we can answer "allow".
//   4. The tool actually runs (a file is written).
//
// Run: node test-smoke.mjs   (from the sidecar/ dir)

import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import fs from 'node:fs'
import os from 'node:os'

const here = dirname(fileURLToPath(import.meta.url))
const testDir = join(os.tmpdir(), 'devdy-sidecar-test')
const outFile = join(testDir, 'out.txt')

fs.rmSync(testDir, { recursive: true, force: true })
fs.mkdirSync(testDir, { recursive: true })

// Make doubly sure we are NOT on API billing for this test.
const env = { ...process.env }
delete env.ANTHROPIC_API_KEY
delete env.ANTHROPIC_AUTH_TOKEN

const child = spawn('node', [join(here, 'index.mjs')], {
  cwd: testDir,
  env,
  stdio: ['pipe', 'pipe', 'inherit'],
})

function sendCmd(obj) {
  child.stdin.write(JSON.stringify(obj) + '\n')
}

let permissionFired = false
let sawResult = false
let buffer = ''

const HARD_TIMEOUT = setTimeout(() => {
  console.error('\n[TIMEOUT] no result within 150s — killing sidecar')
  child.kill('SIGKILL')
  finish(2)
}, 150_000)

function finish(code) {
  clearTimeout(HARD_TIMEOUT)
  const fileOk = fs.existsSync(outFile)
  const content = fileOk ? fs.readFileSync(outFile, 'utf8').trim() : '(missing)'
  console.log('\n==================== SMOKE TEST RESULT ====================')
  console.log('canUseTool fired   :', permissionFired ? 'YES ✅' : 'NO ❌')
  console.log('result received    :', sawResult ? 'YES ✅' : 'NO ❌')
  console.log('out.txt written    :', fileOk ? `YES ✅  ("${content}")` : 'NO ❌')
  console.log('===========================================================')
  const pass = permissionFired && sawResult && fileOk
  console.log(pass ? 'PASS ✅ — sidecar + Agent SDK + canUseTool work on current auth' : 'FAIL ❌')
  try { child.kill() } catch {}
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
    try { msg = JSON.parse(line) } catch { console.log('[non-json]', line); continue }
    handle(msg)
  }
})

function handle(msg) {
  switch (msg.type) {
    case '_devdy_ready':
      console.log('[ready] sending prompt…')
      sendCmd({
        type: 'prompt',
        text:
          'Create a file named out.txt containing exactly the text devdy-sidecar-ok ' +
          'in the current working directory, then stop. Do not ask for confirmation.',
        options: { cwd: testDir, permissionMode: 'default' },
      })
      break
    case '_devdy_permission_request':
      permissionFired = true
      console.log(`[permission] ${msg.tool_name} — ${msg.title ?? ''} → ALLOW`)
      sendCmd({ type: 'permission_response', requestId: msg.requestId, decision: 'allow' })
      break
    case '_devdy_stderr':
      // SDK/CLI stderr — keep quiet unless debugging
      break
    case '_devdy_error':
      console.error('[sidecar_error]', msg.error)
      break
    case '_devdy_done':
      console.log('[done]')
      break
    case '_devdy_closed':
      console.log('[closed]')
      finish(0)
      break
    default: {
      // Raw stream-json SDK message.
      const m = msg
      if (m?.type === 'assistant') {
        const blocks = m.message?.content ?? []
        for (const b of blocks) {
          if (b.type === 'text' && b.text?.trim()) console.log('[assistant]', b.text.trim().slice(0, 120))
          if (b.type === 'tool_use') console.log(`[tool_use] ${b.name}`)
        }
      } else if (m?.type === 'system' && m.subtype === 'init') {
        console.log(`[init] model=${m.model} session=${m.session_id?.slice(0, 8)} tools=${(m.tools || []).length}`)
      } else if (m?.type === 'result') {
        sawResult = true
        console.log(`[result] is_error=${m.is_error} cost=${m.total_cost_usd ?? 'n/a'}`)
        sendCmd({ type: 'end_input' })
      }
      break
    }
  }
}

child.on('exit', (code) => {
  if (!sawResult) {
    console.error(`[child exited early code=${code}]`)
    finish(1)
  }
})
