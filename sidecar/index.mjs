#!/usr/bin/env node
// Devdy — Claude sidecar.
//
// Hosts @anthropic-ai/claude-agent-sdk and bridges it to the Rust broker over
// newline-delimited JSON (NDJSON) on stdin/stdout. One sidecar process == one
// run (one persistent, multi-turn session).
//
// Protocol — IN (stdin, one JSON object per line):
//   { "type": "prompt", "text": "...", "options": { model?, cwd?, resume?,
//        permissionMode?, allowedTools?, includePartialMessages? } }
//     First prompt starts the session; later prompts are follow-up user turns.
//   { "type": "permission_response", "requestId": "...", "decision": "allow"|"deny",
//        "reason"?: "...", "always"?: bool }
//   { "type": "interrupt" }   // ask the agent to stop at a safe boundary
//   { "type": "end_input" }   // no more user turns; let the session finish
//   { "type": "abort" }       // hard exit
//   { "type": "get_usage" }   // request a fresh /usage snapshot now (best-effort)
//
// Protocol — OUT (stdout, one JSON object per line):
//   <raw SDKMessage>           // emitted verbatim (type: system|assistant|user|
//                              // result|stream_event…) so the Rust drain's
//                              // existing stream-json parsing works unchanged.
//   { "type": "_devdy_ready" }
//   { "type": "_devdy_permission_request", requestId, tool_name, tool_input,
//        tool_use_id, title, description, display_name }
//   { "type": "_devdy_stderr", "text": "..." }
//   { "type": "_devdy_usage", usage }  // structured /usage data (plan rate-limit
//                                      // utilization + reset windows), captured at
//                                      // turn end and on the get_usage command
//   { "type": "_devdy_done" }          // query iterator finished
//   { "type": "_devdy_closed" }        // session fully torn down
//   { "type": "_devdy_error", "error": "..." }
//
// Control messages are namespaced with a `_devdy_` prefix so they never collide
// with real stream-json message types.
//
// Resume: if options.resume is absent but DEVDY_RESUME_SESSION is set in env,
// the first prompt resumes that session.

import { query } from '@anthropic-ai/claude-agent-sdk'
import { randomUUID } from 'node:crypto'
import readline from 'node:readline'

function send(obj) {
  process.stdout.write(JSON.stringify(obj) + '\n')
}

// Pull the structured data behind the `/usage` command (session cost + claude.ai
// plan rate-limit utilization windows with real per-account reset times) off the
// live query and forward it. Best-effort: the method is experimental, only
// returns real windows for subscription (OAuth) sessions, and may be unavailable
// or slow at teardown — so it's fully guarded and never throws into the loop.
async function captureUsage() {
  if (usageCaptureInFlight) return
  const q = currentQuery
  const fn = q && q.usage_EXPERIMENTAL_MAY_CHANGE_DO_NOT_RELY_ON_THIS_API_YET
  if (typeof fn !== 'function') return
  usageCaptureInFlight = true
  let timer
  try {
    const usage = await Promise.race([
      fn.call(q),
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error('usage request timed out')), 8000)
      }),
    ])
    if (usage) send({ type: '_devdy_usage', usage })
  } catch (err) {
    send({ type: '_devdy_log', level: 'debug', text: `usage capture skipped: ${String(err)}` })
  } finally {
    if (timer) clearTimeout(timer)
    usageCaptureInFlight = false
  }
}

function requestUsageCapture() {
  const p = captureUsage()
  pendingUsageCaptures.add(p)
  p.finally(() => pendingUsageCaptures.delete(p))
}

async function flushUsageCaptures(timeoutMs = 3000) {
  if (!pendingUsageCaptures.size) return
  await Promise.race([
    Promise.allSettled([...pendingUsageCaptures]),
    new Promise((resolve) => setTimeout(resolve, timeoutMs)),
  ])
}

function startUsagePolling(intervalMs = 60_000) {
  if (usagePollTimer) return
  usagePollTimer = setInterval(() => requestUsageCapture(), intervalMs)
}

function stopUsagePolling() {
  if (!usagePollTimer) return
  clearInterval(usagePollTimer)
  usagePollTimer = null
}

// Async queue feeding the SDK's streaming-input prompt. Staying open across
// turns is what enables multi-turn + the canUseTool callback.
class MessageQueue {
  constructor() {
    this.items = []
    this.resolvers = []
    this.closed = false
  }
  push(item) {
    const r = this.resolvers.shift()
    if (r) r({ value: item, done: false })
    else this.items.push(item)
  }
  close() {
    this.closed = true
    let r
    while ((r = this.resolvers.shift())) r({ value: undefined, done: true })
  }
  [Symbol.asyncIterator]() {
    return {
      next: () => {
        if (this.items.length) return Promise.resolve({ value: this.items.shift(), done: false })
        if (this.closed) return Promise.resolve({ value: undefined, done: true })
        return new Promise((resolve) => this.resolvers.push(resolve))
      },
    }
  }
}

// `images` is an array of { media_type, data(base64) }. With images, content
// becomes a block array the Agent SDK understands; otherwise a plain string.
function userMessage(text, images) {
  let content = text
  if (Array.isArray(images) && images.length) {
    content = []
    if (text) content.push({ type: 'text', text })
    for (const img of images) {
      content.push({
        type: 'image',
        source: { type: 'base64', media_type: img.media_type, data: img.data },
      })
    }
  }
  return { type: 'user', message: { role: 'user', content }, parent_tool_use_id: null }
}

const pendingPermissions = new Map() // requestId -> resolve fn
let inputQueue = null
let currentQuery = null
let started = false
let usageCaptureInFlight = false
let usagePollTimer = null
const pendingUsageCaptures = new Set()

function startQuery(firstText, opts = {}, firstImages) {
  inputQueue = new MessageQueue()
  inputQueue.push(userMessage(firstText, firstImages))

  const canUseTool = async (toolName, input, options) => {
    const requestId = randomUUID()
    send({
      type: '_devdy_permission_request',
      requestId,
      tool_name: toolName,
      tool_input: input,
      tool_use_id: options?.toolUseID ?? null,
      title: options?.title ?? null,
      description: options?.description ?? null,
      display_name: options?.displayName ?? null,
    })
    const decision = await new Promise((resolve) => pendingPermissions.set(requestId, resolve))
    // canUseTool can only allow or deny; anything other than an explicit
    // "allow" (including the legacy "ask") is treated as deny.
    if (decision.decision === 'allow') {
      const result = { behavior: 'allow', updatedInput: input }
      // AskUserQuestion is answered through updatedInput: the user's selections
      // are folded into the tool input so the tool echoes them as its `answers`
      // output back to the model. (PermissionResult has no separate result channel.)
      if (decision.answers && typeof decision.answers === 'object') {
        result.updatedInput = { ...input, answers: decision.answers }
        if (typeof decision.response === 'string' && decision.response) {
          result.updatedInput.response = decision.response
        }
      }
      if (decision.always && Array.isArray(options?.suggestions)) {
        result.updatedPermissions = options.suggestions
      }
      return result
    }
    return { behavior: 'deny', message: decision.reason || 'Denied by user' }
  }

  const options = {
    canUseTool,
    permissionMode: opts.permissionMode || process.env.DEVDY_PERMISSION_MODE || 'default',
    includePartialMessages: !!opts.includePartialMessages,
    stderr: (data) => send({ type: '_devdy_stderr', text: data }),
  }
  // Model: explicit per-turn option wins, else the broker-provided env default.
  if (opts.model) options.model = opts.model
  else if (process.env.DEVDY_MODEL) options.model = process.env.DEVDY_MODEL
  if (opts.cwd) options.cwd = opts.cwd
  // Centrally-managed MCP servers injected per run (Record<name, config>).
  // MCP tools still flow through canUseTool → the permission modal is unchanged.
  if (opts.mcpServers && typeof opts.mcpServers === 'object') options.mcpServers = opts.mcpServers
  const resume = opts.resume || process.env.DEVDY_RESUME_SESSION
  if (resume) options.resume = resume
  if (Array.isArray(opts.allowedTools)) options.allowedTools = opts.allowedTools
  // Pin to a specific claude binary when the broker provides one; otherwise the
  // SDK uses its bundled CLI (both read the same macOS Keychain login).
  if (process.env.DEVDY_CLAUDE_PATH) options.pathToClaudeCodeExecutable = process.env.DEVDY_CLAUDE_PATH
  // GĐ6 (AC1): per-project git account context. Keep the default Claude Code
  // preset system prompt and APPEND our context (a plain string would replace
  // the whole prompt and lose tool guidance).
  if (opts.appendSystemPrompt) {
    options.systemPrompt = {
      type: 'preset',
      preset: 'claude_code',
      append: String(opts.appendSystemPrompt),
    }
  }
  // 'warning' mode additionally polls /usage on an interval WHILE a run streams
  // (used when already near the plan limit). In every mode we still capture once
  // at `init` and once at `result` — piggybacking on this run's already-open
  // control channel is a near-free way to keep the plan % fresh (no extra
  // process, no completion tokens), so normal runs update the badge too.
  const usageCaptureMode = opts.usageCaptureMode || process.env.DEVDY_USAGE_CAPTURE_MODE || 'normal'
  const pollWhileRunning = usageCaptureMode === 'warning'
  const configuredUsagePollMs = Number(opts.usagePollMs || process.env.DEVDY_USAGE_POLL_MS || 60_000)
  const usagePollMs = Number.isFinite(configuredUsagePollMs) && configuredUsagePollMs >= 30_000
    ? configuredUsagePollMs
    : 60_000

  ;(async () => {
    try {
      currentQuery = query({ prompt: inputQueue, options })
      if (pollWhileRunning) startUsagePolling(usagePollMs)
      for await (const message of currentQuery) {
        // Emit raw stream-json so the Rust drain parses it like the CLI output.
        process.stdout.write(JSON.stringify(message) + '\n')
        // Snapshot plan usage once the session is live (at init the CLI is up
        // and the control channel works; by `result` it's already tearing down).
        // Fire-and-forget — NEVER await here: the control response is multiplexed
        // onto THIS message stream, so awaiting blocks its own delivery and
        // deadlocks until the query closes. The loop must keep pulling, which is
        // exactly what drives the response back to captureUsage().
        if (message && message.type === 'system' && message.subtype === 'init') requestUsageCapture()
        // Refresh once more at turn end so the app-wide usage warning reflects
        // the tokens just consumed. Best-effort: may miss if the session tears
        // down first; captureUsage()'s single-flight dedupes with the init call.
        if (message && message.type === 'result') requestUsageCapture()
      }
      stopUsagePolling()
      await flushUsageCaptures()
      send({ type: '_devdy_done' })
    } catch (err) {
      send({ type: '_devdy_error', error: String(err && err.stack ? err.stack : err) })
    } finally {
      stopUsagePolling()
      await flushUsageCaptures()
      send({ type: '_devdy_closed' })
    }
  })()
}

function startUsageProbe(opts = {}) {
  inputQueue = new MessageQueue()
  inputQueue.push(userMessage('/usage'))

  const options = {
    permissionMode: opts.permissionMode || process.env.DEVDY_PERMISSION_MODE || 'default',
    includePartialMessages: false,
    stderr: (data) => send({ type: '_devdy_stderr', text: data }),
  }
  if (opts.cwd) options.cwd = opts.cwd
  if (process.env.DEVDY_CLAUDE_PATH) options.pathToClaudeCodeExecutable = process.env.DEVDY_CLAUDE_PATH

  ;(async () => {
    try {
      currentQuery = query({ prompt: inputQueue, options })
      let closeScheduled = false
      let usageRequested = false
      const closeAfterUsageCapture = () => {
        if (closeScheduled) return
        closeScheduled = true
        ;(async () => {
          await flushUsageCaptures(10_000)
          if (inputQueue) inputQueue.close()
        })()
      }
      const requestUsageOnce = () => {
        if (usageRequested) return
        usageRequested = true
        requestUsageCapture()
        closeAfterUsageCapture()
      }
      for await (const message of currentQuery) {
        process.stdout.write(JSON.stringify(message) + '\n')
        if (message && message.type === 'system' && message.subtype === 'init') {
          requestUsageOnce()
        }
        if (message && message.type === 'result') {
          requestUsageOnce()
        }
      }
      await flushUsageCaptures()
      send({ type: '_devdy_done' })
    } catch (err) {
      send({ type: '_devdy_error', error: String(err && err.stack ? err.stack : err) })
    } finally {
      await flushUsageCaptures()
      send({ type: '_devdy_closed' })
    }
  })()
}

const rl = readline.createInterface({ input: process.stdin })
rl.on('line', (raw) => {
  const line = raw.trim()
  if (!line) return
  let cmd
  try {
    cmd = JSON.parse(line)
  } catch {
    return
  }
  switch (cmd.type) {
    case 'prompt':
      if (!started) {
        started = true
        startQuery(cmd.text ?? '', cmd.options || {}, cmd.images)
      } else if (inputQueue) {
        inputQueue.push(userMessage(cmd.text ?? '', cmd.images))
      }
      break
    case 'permission_response': {
      const resolve = pendingPermissions.get(cmd.requestId)
      if (resolve) {
        pendingPermissions.delete(cmd.requestId)
        resolve(cmd)
      }
      break
    }
    case 'interrupt':
      if (currentQuery && typeof currentQuery.interrupt === 'function') {
        currentQuery.interrupt().catch((e) => send({ type: '_devdy_stderr', text: String(e) }))
      }
      break
    case 'end_input':
      if (inputQueue) inputQueue.close()
      break
    case 'get_usage':
      captureUsage()
      break
    case 'usage_probe':
      if (!started) {
        started = true
        startUsageProbe(cmd.options || {})
      } else {
        requestUsageCapture()
      }
      break
    case 'abort':
      process.exit(0)
      break
    default:
      break
  }
})

rl.on('close', () => {
  // stdin closed by the broker → end the session input.
  if (inputQueue) inputQueue.close()
})

send({ type: '_devdy_ready' })
