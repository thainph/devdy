#!/usr/bin/env node
// Deterministic sidecar tests (no CLI, no network): the Agent SDK is replaced by
// test-fixtures/mock-sdk.mjs via a module hook. Covers the permission channel's
// no-hang guarantees and the liveness protocol.
//
// Run: node test-mocked.mjs   (from the sidecar/ dir)

import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import assert from 'node:assert/strict'

const here = dirname(fileURLToPath(import.meta.url))

// Spawn the sidecar under `scenario`; `onMessage(msg, ctl)` drives it. Resolves
// with every message once the process exits (or rejects on timeout = a hang).
function runSidecar(scenario, onMessage, timeoutMs = 10_000, extraEnv = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(
      process.execPath,
      ['--import', join(here, 'test-fixtures', 'mock-sdk-register.mjs'), join(here, 'index.mjs')],
      {
        cwd: here,
        env: { ...process.env, MOCK_SCENARIO: scenario, DEVDY_SIDECAR_HEARTBEAT_MS: '100', ...extraEnv },
        stdio: ['pipe', 'pipe', 'inherit'],
      },
    )
    const messages = []
    const ctl = {
      send: (obj) => child.stdin.write(JSON.stringify(obj) + '\n'),
      closeInput: () => child.stdin.end(),
    }
    const timer = setTimeout(() => {
      child.kill('SIGKILL')
      reject(new Error(`[${scenario}] sidecar did not exit within ${timeoutMs}ms (hang). Saw: ${messages.map((m) => m.type).join(', ')}`))
    }, timeoutMs)
    let buf = ''
    child.stdout.on('data', (chunk) => {
      buf += chunk.toString()
      let i
      while ((i = buf.indexOf('\n')) >= 0) {
        const line = buf.slice(0, i)
        buf = buf.slice(i + 1)
        if (!line.trim()) continue
        const msg = JSON.parse(line)
        messages.push(msg)
        onMessage(msg, ctl, messages)
      }
    })
    child.on('exit', (code) => {
      clearTimeout(timer)
      resolve({ messages, code })
    })
  })
}

const startPrompt = (ctl) => ctl.send({ type: 'prompt', text: 'go', options: { permissionMode: 'default' } })
const of = (messages, type) => messages.filter((m) => m.type === type)

const tests = {
  // Liveness protocol: ready declares the heartbeat, and heartbeats flow.
  async 'ready declares heartbeat capability and heartbeats arrive'() {
    const { messages } = await runSidecar('permission', (msg, ctl, all) => {
      if (msg.type === '_devdy_ready') startPrompt(ctl)
      if (msg.type === '_devdy_permission_request') {
        // Hold the answer until a couple of heartbeats went by.
        const wait = setInterval(() => {
          if (of(all, '_devdy_heartbeat').length >= 2) {
            clearInterval(wait)
            ctl.send({ type: 'permission_response', requestId: msg.requestId, decision: 'allow' })
            ctl.closeInput()
          }
        }, 20)
      }
    })
    const ready = of(messages, '_devdy_ready')[0]
    assert.equal(ready.capabilities?.heartbeat_ms, 100)
    assert.ok(of(messages, '_devdy_heartbeat').length >= 2)
    assert.equal(of(messages, 'result')[0].mock_behavior, 'allow')
  },

  // Control: a normal answer is delivered and the process exits on input close.
  async 'answered request is delivered; session closes and exits'() {
    const { messages, code } = await runSidecar('permission', (msg, ctl) => {
      if (msg.type === '_devdy_ready') startPrompt(ctl)
      if (msg.type === '_devdy_permission_request') {
        ctl.send({ type: 'permission_response', requestId: msg.requestId, decision: 'deny', reason: 'no thanks' })
        ctl.closeInput()
      }
    })
    const result = of(messages, 'result')[0]
    assert.equal(result.mock_behavior, 'deny')
    assert.equal(result.mock_message, 'no thanks')
    assert.equal(of(messages, '_devdy_permission_cancelled').length, 0)
    assert.equal(of(messages, '_devdy_closed').length, 1)
    assert.equal(of(messages, '_devdy_closed')[0].code, 0)
    assert.equal(code, 0)
  },

  // A failed session reports it on the close line AND in the exit code, so the
  // broker marks the run failed even if it stops waiting on the pipes.
  async 'failed session closes with code 1 and exits 1'() {
    const { messages, code } = await runSidecar('throw', (msg, ctl) => {
      if (msg.type === '_devdy_ready') startPrompt(ctl)
    })
    assert.equal(of(messages, '_devdy_error').length, 1)
    assert.equal(of(messages, '_devdy_closed')[0].code, 1)
    assert.equal(code, 1)
  },

  // The original hang: stdin closes while a request is open. The sidecar must
  // settle it itself (deny + cancelled), finish the turn, close and exit.
  async 'input closed while a request is open → denied, cancelled, exits'() {
    let requestId = null
    const { messages, code } = await runSidecar('permission', (msg, ctl) => {
      if (msg.type === '_devdy_ready') startPrompt(ctl)
      if (msg.type === '_devdy_permission_request') {
        requestId = msg.requestId
        ctl.closeInput() // never answer
      }
    })
    const cancelled = of(messages, '_devdy_permission_cancelled')
    assert.equal(cancelled.length, 1)
    assert.equal(cancelled[0].requestId, requestId)
    assert.equal(cancelled[0].level, 'warn')
    const result = of(messages, 'result')[0]
    assert.equal(result.mock_behavior, 'deny')
    assert.match(result.mock_message, /input channel closed/)
    assert.equal(of(messages, '_devdy_closed').length, 1)
    assert.equal(code, 0)
  },

  // A tool asking AFTER stdin closed is denied at once — no prompt is raised
  // that nobody could ever answer.
  async 'request after input closed is denied immediately, no prompt'() {
    const { messages, code } = await runSidecar('after-close', (msg, ctl) => {
      if (msg.type === '_devdy_ready') {
        startPrompt(ctl)
        ctl.closeInput()
      }
    })
    assert.equal(of(messages, '_devdy_permission_request').length, 0)
    const result = of(messages, 'result')[0]
    assert.equal(result.mock_tool, 'Edit')
    assert.equal(result.mock_behavior, 'deny')
    assert.ok(of(messages, '_devdy_log').some((m) => m.level === 'warn' && /Edit was denied/.test(m.text)))
    assert.equal(code, 0)
  },

  // The CLI withdraws the request (interrupt / tool aborted): the prompt is
  // dropped instead of lingering forever.
  async 'CLI-withdrawn request is cancelled (info) and settled'() {
    let requestId = null
    const { messages } = await runSidecar('abort', (msg, ctl) => {
      if (msg.type === '_devdy_ready') startPrompt(ctl)
      if (msg.type === '_devdy_permission_request') requestId = msg.requestId
      if (msg.type === 'result') ctl.closeInput()
    })
    const cancelled = of(messages, '_devdy_permission_cancelled')
    assert.equal(cancelled.length, 1)
    assert.equal(cancelled[0].requestId, requestId)
    assert.equal(cancelled[0].level, 'info')
    assert.equal(of(messages, 'result')[0].mock_behavior, 'deny')
  },

  // Translation engine: a no-tools session strips every tool source and denies
  // any call without a prompt — even with looser options and MCP env fallbacks.
  async 'noTools session has no tools and denies every call without a prompt'() {
    const { messages, code } = await runSidecar('permission', (msg, ctl) => {
      if (msg.type === '_devdy_ready') {
        ctl.send({
          type: 'prompt',
          text: 'go',
          options: {
            noTools: true,
            permissionMode: 'bypassPermissions',
            allowedTools: ['Bash', 'mcp__devdy'],
            mcpServers: { devdy: { type: 'http', url: 'http://127.0.0.1:1/mcp' } },
          },
        })
      }
      if (msg.type === 'result') ctl.closeInput()
    }, 10_000, { DEVDY_MCP_SERVERS: JSON.stringify({ env: { type: 'stdio', command: 'x' } }), DEVDY_ALLOWED_TOOLS: '["Bash"]' })
    const init = messages.find((m) => m.type === 'system' && m.subtype === 'init')
    const o = init.mock_options
    assert.deepEqual(o.tools, [])
    assert.deepEqual(o.allowedTools, [])
    assert.deepEqual(o.mcpServers, {})
    assert.deepEqual(o.skills, [])
    assert.equal(o.strictMcpConfig, true)
    assert.equal(o.permissionMode, 'dontAsk')
    assert.equal(init.mock_has_can_use_tool, true)
    assert.equal(of(messages, '_devdy_permission_request').length, 0)
    const result = of(messages, 'result')[0]
    assert.equal(result.mock_tool, 'Write')
    assert.equal(result.mock_behavior, 'deny')
    assert.match(result.mock_message, /no tool access/)
    assert.equal(code, 0)
  },

  // Control: a regular session keeps the default tool set.
  async 'regular session does not restrict tools'() {
    const { messages } = await runSidecar('permission', (msg, ctl) => {
      if (msg.type === '_devdy_ready') startPrompt(ctl)
      if (msg.type === '_devdy_permission_request') {
        ctl.send({ type: 'permission_response', requestId: msg.requestId, decision: 'allow' })
        ctl.closeInput()
      }
    })
    const o = messages.find((m) => m.type === 'system' && m.subtype === 'init').mock_options
    assert.equal(o.tools, undefined)
    assert.equal(o.strictMcpConfig, undefined)
    assert.equal(o.permissionMode, 'default')
    // Keychain reads through the `security` CLI are denied in every session.
    assert.ok(o.disallowedTools.includes('Bash(security find-generic-password:*)'))
  },
}

let failed = 0
for (const [name, fn] of Object.entries(tests)) {
  try {
    await fn()
    console.log(`ok   ${name}`)
  } catch (e) {
    failed++
    console.log(`FAIL ${name}\n     ${e.message}`)
  }
}
console.log(failed ? `\n${failed} failed` : `\nall ${Object.keys(tests).length} passed`)
process.exit(failed ? 1 : 0)
