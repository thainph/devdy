#!/usr/bin/env node
// Round-trip test for AskUserQuestion answering via canUseTool.
//
// Verifies the core assumption behind the in-app question modal: when the model
// calls the built-in AskUserQuestion tool, we answer it by returning
// `behavior:'allow'` with `updatedInput.answers` (the only channel a
// PermissionResult exposes), and the model then RECEIVES the chosen answer.
//
// Strategy: prompt Claude to ask a single question via AskUserQuestion, answer
// it with a distinctive sentinel label, and check that the model's final reply
// echoes the sentinel — proving the answer round-tripped through updatedInput.
//
// Run: node test-askuserquestion.mjs   (from the sidecar/ dir)

import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import fs from 'node:fs'
import os from 'node:os'

const here = dirname(fileURLToPath(import.meta.url))
const testDir = join(os.tmpdir(), 'devdy-sidecar-auq-test')
fs.mkdirSync(testDir, { recursive: true })

const SENTINEL = 'magenta-sentinel-42'

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

let askFired = false
let answeredWith = null
let sawResult = false
let finalText = ''
let buffer = ''

const HARD_TIMEOUT = setTimeout(() => {
  console.error('\n[TIMEOUT] no result within 150s — killing sidecar')
  child.kill('SIGKILL')
  finish()
}, 150_000)

function finish() {
  clearTimeout(HARD_TIMEOUT)
  const echoed = finalText.toLowerCase().includes(SENTINEL)
  console.log('\n=============== ASK-USER-QUESTION TEST RESULT ===============')
  console.log('AskUserQuestion fired :', askFired ? 'YES ✅' : 'NO ❌ (model never asked)')
  console.log('answered with         :', answeredWith ? JSON.stringify(answeredWith) : '(none)')
  console.log('result received       :', sawResult ? 'YES ✅' : 'NO ❌')
  console.log('model echoed answer   :', echoed ? `YES ✅ (saw "${SENTINEL}")` : 'NO ❌')
  console.log('final text            :', finalText.slice(0, 200))
  console.log('=============================================================')
  const pass = askFired && sawResult && echoed
  console.log(
    pass
      ? 'PASS ✅ — AskUserQuestion answers round-trip via updatedInput'
      : 'FAIL ❌ — see above (if "fired: NO", the model just chose not to ask; rerun)',
  )
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
    try { msg = JSON.parse(line) } catch { continue }
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
          'Use the AskUserQuestion tool to ask me to choose a code name. ' +
          `Offer exactly these two options: "${SENTINEL}" and "teal-decoy-99". ` +
          'After I answer, reply with a single sentence stating verbatim the code name I chose.',
        options: { cwd: testDir, permissionMode: 'default' },
      })
      break
    case '_devdy_permission_request':
      if (msg.tool_name === 'AskUserQuestion') {
        askFired = true
        // Build the answers map keyed by each question's text → the sentinel label.
        const questions = Array.isArray(msg.tool_input?.questions) ? msg.tool_input.questions : []
        const answers = {}
        for (const q of questions) answers[q.question] = SENTINEL
        answeredWith = answers
        console.log(`[ask] ${questions.length} question(s) → answering ${SENTINEL}`)
        sendCmd({ type: 'permission_response', requestId: msg.requestId, decision: 'allow', answers })
      } else {
        // Anything else (shouldn't happen here) — just allow.
        sendCmd({ type: 'permission_response', requestId: msg.requestId, decision: 'allow' })
      }
      break
    case '_devdy_error':
      console.error('[sidecar_error]', msg.error)
      break
    case '_devdy_closed':
      finish()
      break
    default: {
      const m = msg
      if (m?.type === 'assistant') {
        for (const b of m.message?.content ?? []) {
          if (b.type === 'text' && b.text?.trim()) {
            finalText = b.text.trim()
            console.log('[assistant]', finalText.slice(0, 160))
          }
          if (b.type === 'tool_use') console.log(`[tool_use] ${b.name}`)
        }
      } else if (m?.type === 'result') {
        sawResult = true
        console.log(`[result] is_error=${m.is_error}`)
        sendCmd({ type: 'end_input' })
      }
      break
    }
  }
}

child.on('exit', (code) => {
  if (!sawResult) {
    console.error(`[child exited early code=${code}]`)
    finish()
  }
})
