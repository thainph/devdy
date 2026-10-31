#!/usr/bin/env node
// Devdy — Claude sidecar.
//
// Hosts @anthropic-ai/claude-agent-sdk and bridges it to the Rust broker over
// newline-delimited JSON (NDJSON) on stdin/stdout. One sidecar process == one
// run (one persistent, multi-turn session).
//
// Protocol — IN (stdin, one JSON object per line):
//   { "type": "prompt", "text": "...", "options": { model?, cwd?, resume?,
//        permissionMode?, allowedTools?, includePartialMessages?, noTools? } }
//     noTools: true starts a session with NO tools at all (see applyNoTools).
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
//   { "type": "_devdy_ready", "capabilities": { "heartbeat_ms": 30000 } }
//   { "type": "_devdy_permission_request", requestId, tool_name, tool_input,
//        tool_use_id, title, description, display_name }
//   { "type": "_devdy_stderr", "text": "..." }
//   { "type": "_devdy_usage", usage }  // structured /usage data (plan rate-limit
//                                      // utilization + reset windows), captured at
//                                      // turn end and on the get_usage command
//   { "type": "_devdy_done" }          // query iterator finished
//   { "type": "_devdy_closed", "code": 0|1 }  // session fully torn down (the
//                                      // process exits right after, with `code`)
//   { "type": "_devdy_error", "error": "..." }
//   { "type": "_devdy_heartbeat" }     // every 30s: proves this bridge is alive
//   { "type": "_devdy_permission_cancelled", requestId, level, text }
//                                      // a request settled here (CLI cancelled
//                                      // it, or no answer can arrive any more)
//
// Control messages are namespaced with a `_devdy_` prefix so they never collide
// with real stream-json message types.
//
// Resume: if options.resume is absent but DEVDY_RESUME_SESSION is set in env,
// the first prompt resumes that session.

import { query } from '@anthropic-ai/claude-agent-sdk'
import { randomUUID } from 'node:crypto'
import readline from 'node:readline'
import { readFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'

// --- Orphan guard ------------------------------------------------------------
// Devdy spawns this sidecar as the leader of its own process group and says so
// with DEVDY_OWN_PROCESS_GROUP=1 (removed here so no child inherits it). If
// Devdy dies without stopping us (crash, force quit), we are re-parented and
// nobody reads our output any more: tear the whole group down (CLI, MCP
// servers, tool processes) rather than letting a turn run on, burning tokens,
// with nobody listening. Processes that left the group are listed by Devdy's
// background-process view at its next launch.
const OWN_GROUP = process.env.DEVDY_OWN_PROCESS_GROUP === '1'
delete process.env.DEVDY_OWN_PROCESS_GROUP
const PARENT_PID = process.ppid
let orphaned = false
function orphanTeardown() {
  if (orphaned) return
  orphaned = true
  // Survive our own group-wide SIGTERM long enough to follow up with SIGKILL.
  process.on('SIGTERM', () => {})
  try { process.kill(-process.pid, 'SIGTERM') } catch {}
  setTimeout(() => {
    try { process.kill(-process.pid, 'SIGKILL') } catch {}
    process.exit(1)
  }, 2000)
}
const parentGone = () => process.ppid !== PARENT_PID
if (OWN_GROUP && PARENT_PID > 1) {
  const watch = setInterval(() => {
    if (parentGone()) { clearInterval(watch); orphanTeardown() }
  }, Math.max(50, Number(process.env.DEVDY_PARENT_WATCH_MS) || 2000))
  watch.unref()
}
// A write to a closed stdout (EPIPE) is how the death of Devdy usually shows
// first; the re-parenting follows right after. Re-check then: orphaned → tear
// down, otherwise stop as before (the reader went away on purpose).
process.stdout.on('error', (err) => {
  if (err?.code !== 'EPIPE') throw err
  setTimeout(() => {
    if (OWN_GROUP && parentGone()) orphanTeardown()
    else process.exit(1)
  }, 300)
})

// Broker → sidecar inputs consumed by this process only. Captured once and
// removed from process.env so they never reach the Claude CLI child (or the MCP
// servers it spawns) — DEVDY_MCP_SERVERS carries the session's bearer token.
const SIDECAR_ENV = {}
for (const k of ['DEVDY_MCP_SERVERS', 'DEVDY_ALLOWED_TOOLS', 'DEVDY_APPEND_SYSTEM_PROMPT']) {
  SIDECAR_ENV[k] = process.env[k]
  delete process.env[k]
}

function send(obj) {
  process.stdout.write(JSON.stringify(obj) + '\n')
}

// --- Global safety net -------------------------------------------------------
// Surface EVERY uncaught failure to the user as a friendly `_devdy_error` (which
// the broker forwards to the run log as an error line) instead of letting Node
// print a raw stack trace to stderr and exit — so the UI shows a clear message
// rather than the process appearing to "crash".
function reportFatal(err, context) {
  const msg = String(err && err.stack ? err.stack : err)
  try { send({ type: '_devdy_error', error: context ? `${context}: ${msg}` : msg }) } catch {}
}
process.on('uncaughtException', (err) => { reportFatal(err, 'Uncaught exception'); setTimeout(() => process.exit(1), 50) })
process.on('unhandledRejection', (err) => { reportFatal(err, 'Unhandled rejection'); setTimeout(() => process.exit(1), 50) })

// Liveness heartbeat. Driven by a plain timer — independent of the model or any
// tool — so the broker can tell a quiet-but-working session (heartbeats keep
// coming) from a dead or frozen bridge (they stop). unref: never keeps the
// process alive on its own.
// The interval is declared to the broker in `_devdy_ready.capabilities`;
// DEVDY_SIDECAR_HEARTBEAT_MS exists only so tests can run fast.
const HEARTBEAT_MS = Math.max(50, Number(process.env.DEVDY_SIDECAR_HEARTBEAT_MS) || 30_000)
setInterval(() => send({ type: '_devdy_heartbeat' }), HEARTBEAT_MS).unref()

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
// Set once the broker closed our stdin: no permission answer can ever arrive
// after that, so waiting on one would hang the turn forever.
let inputClosed = false
let inputQueue = null

const INPUT_CLOSED_REASON =
  'Devdy can no longer show permission prompts for this session (its input channel closed). ' +
  'Do not retry; finish the turn and tell the user to send a follow-up message to continue.'

// Settle a pending request here (not via the broker) and tell the broker to drop
// the prompt it is showing.
function cancelPermission(requestId, reason, level, text) {
  const resolve = pendingPermissions.get(requestId)
  if (!resolve) return
  pendingPermissions.delete(requestId)
  resolve({ decision: 'deny', reason })
  send({ type: '_devdy_permission_cancelled', requestId, level, text })
}
const NO_TOOLS_REASON = 'This session has no tool access.'

// Agents never read the macOS Keychain through the `security` CLI: Devdy's item
// holds PATs / tokens (and the sealed secret notes vault). Deny rules hold in
// every permission mode, bypass included. A determined agent can still go around
// a prefix rule (another binary, a script), which is why secret notes are also
// encrypted under a password only the user knows.
const KEYCHAIN_DENY_RULES = [
  'Bash(security find-generic-password:*)',
  'Bash(security find-internet-password:*)',
  'Bash(security dump-keychain:*)',
  'Bash(security export:*)',
]

// Lock a session's options down to text-only: no built-in tools, no MCP servers
// (passed or from settings/plugins), no skills, and any tool call that still
// reaches the permission layer is denied — never auto-approved. Used for the
// translation engine, whose prompts carry UNTRUSTED text (Slack messages, GitHub
// comments) that could otherwise prompt-inject a Bash/Write/WebFetch call.
// Overrides whatever the caller or env fallbacks set.
function applyNoTools(options) {
  options.tools = []
  options.allowedTools = []
  options.mcpServers = {}
  options.strictMcpConfig = true
  options.skills = []
  // 'dontAsk' denies anything not pre-approved instead of prompting; the
  // callback is a second wall in case a call still asks.
  options.permissionMode = 'dontAsk'
  options.canUseTool = async () => ({ behavior: 'deny', message: NO_TOOLS_REASON })
  delete options.allowDangerouslySkipPermissions
  return options
}

// Claude Code reads AGENTS.md (Codex's file) when a project has no CLAUDE.md. Where every
// AGENTS.md from cwd up to the root holds nothing but Devdy-written blocks, those only duplicate
// the rules Claude already gets from .claude/rules — so turn the fallback off for that session.
// The switch drops ALL AGENTS.md files, so any hand-written content keeps it on (the Devdy app
// warns about the duplication instead).
const DEVDY_BLOCK = /<!-- devdy:([\w-]+):([\w-]+) START -->[\s\S]*?<!-- devdy:\1:\2 END -->/g
function devdyManagesAgentsMd(cwd) {
  let dir = resolve(cwd || process.cwd())
  let sawDevdyBlocks = false
  for (;;) {
    let text = null
    try {
      text = readFileSync(join(dir, 'AGENTS.md'), 'utf8')
    } catch { /* no AGENTS.md here */ }
    if (text !== null) {
      const rest = text.replace(DEVDY_BLOCK, '')
      if (rest.trim() !== '') return false
      if (rest !== text) sawDevdyBlocks = true
    }
    const parent = dirname(dir)
    if (parent === dir) return sawDevdyBlocks
    dir = parent
  }
}
function claudeMdOnly(options) {
  options.settings = {
    pluginConfigs: { 'agents-md@builtin': { options: { instructionFiles: 'claude-md' } } },
  }
}

let currentQuery = null
let started = false
let usageCaptureInFlight = false
let usagePollTimer = null
const pendingUsageCaptures = new Set()

function startQuery(firstText, opts = {}, firstImages) {
  inputQueue = new MessageQueue()
  inputQueue.push(userMessage(firstText, firstImages))

  // Auto-allowed tools (e.g. a conductor's own `mcp__conductor`). Bare names are
  // matched here inside canUseTool rather than passed to the SDK as
  // allowedTools: bare allowedTools entries shadow the callback and make the SDK
  // warn (CLAUDE_SDK_CAN_USE_TOOL_SHADOWED). Scoped rules like `Bash(git:*)`
  // still go to the SDK as allowedTools.
  let allowedTools = null
  if (Array.isArray(opts.allowedTools)) allowedTools = opts.allowedTools
  else if (SIDECAR_ENV.DEVDY_ALLOWED_TOOLS) {
    // Resume fallback (like DEVDY_MCP_SERVERS): a conductor's own `mcp__conductor`
    // tools stay auto-allowed in gated permission modes.
    try {
      const parsed = JSON.parse(SIDECAR_ENV.DEVDY_ALLOWED_TOOLS)
      if (Array.isArray(parsed)) allowedTools = parsed
    } catch { /* ignore malformed env */ }
  }
  const autoAllowNames = (allowedTools || []).filter((t) => typeof t === 'string' && !t.includes('('))
  const scopedAllowRules = (allowedTools || []).filter((t) => typeof t === 'string' && t.includes('('))
  // `mcp__server` covers every tool of that server (`mcp__server__tool`).
  const isAutoAllowed = (toolName) =>
    autoAllowNames.some((n) => toolName === n || (n.startsWith('mcp__') && toolName.startsWith(n + '__')))

  const canUseTool = async (toolName, input, options) => {
    if (isAutoAllowed(toolName)) return { behavior: 'allow', updatedInput: input }
    if (inputClosed) {
      send({ type: '_devdy_log', level: 'warn', text: `⚠ ${toolName} was denied: the session can no longer receive permission answers.` })
      return { behavior: 'deny', message: INPUT_CLOSED_REASON }
    }
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
    const decision = await new Promise((resolve) => {
      pendingPermissions.set(requestId, resolve)
      // The CLI withdrew the request (turn interrupted, tool aborted): nobody
      // will act on an answer any more, so settle it and drop the prompt.
      const signal = options?.signal
      if (signal) {
        const onAbort = () => cancelPermission(requestId, 'Request cancelled', 'info', `Permission request for ${toolName} was withdrawn.`)
        if (signal.aborted) onAbort()
        else signal.addEventListener('abort', onAbort, { once: true })
      }
    })
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
      return result
    }
    return { behavior: 'deny', message: decision.reason || 'Denied by user' }
  }

  const permissionMode = opts.permissionMode || process.env.DEVDY_PERMISSION_MODE || 'default'
  const options = {
    permissionMode,
    // Per-turn option wins; else the broker's env default (session runs stream
    // deltas, also on resume where the first prompt carries no options).
    includePartialMessages:
      opts.includePartialMessages ?? process.env.DEVDY_PARTIAL_MESSAGES === '1',
    stderr: (data) => send({ type: '_devdy_stderr', text: data }),
  }
  // 'bypassPermissions' auto-approves every tool call before canUseTool is ever
  // consulted, so passing the callback there only triggers the SDK's
  // CLAUDE_SDK_CAN_USE_TOOL_SHADOWED warning without ever running. Only wire the
  // permission modal when the mode can actually gate a call (e.g. conductor
  // sessions run in bypass and never need it).
  if (permissionMode !== 'bypassPermissions') options.canUseTool = canUseTool
  options.disallowedTools = KEYCHAIN_DENY_RULES
  // Model: explicit per-turn option wins, else the broker-provided env default.
  if (opts.model) options.model = opts.model
  else if (process.env.DEVDY_MODEL) options.model = process.env.DEVDY_MODEL
  if (opts.cwd) options.cwd = opts.cwd
  if (devdyManagesAgentsMd(opts.cwd)) claudeMdOnly(options)
  // Centrally-managed MCP servers injected per run (Record<name, config>).
  // MCP tools still flow through canUseTool → the permission modal is unchanged.
  // Per-turn option wins; else fall back to DEVDY_MCP_SERVERS (set by the broker
  // on RESUME, whose first prompt arrives via send_user_message without options —
  // without this fallback a resumed session loses all its MCP servers).
  if (opts.mcpServers && typeof opts.mcpServers === 'object') {
    options.mcpServers = opts.mcpServers
  } else if (SIDECAR_ENV.DEVDY_MCP_SERVERS) {
    try {
      const parsed = JSON.parse(SIDECAR_ENV.DEVDY_MCP_SERVERS)
      if (parsed && typeof parsed === 'object') options.mcpServers = parsed
    } catch { /* ignore malformed env; run without MCP rather than crash */ }
  }
  const resume = opts.resume || process.env.DEVDY_RESUME_SESSION
  if (resume) options.resume = resume
  // Bare names are handled by canUseTool (see isAutoAllowed); only scoped rules
  // reach the SDK. An explicit empty list is kept as-is.
  if (allowedTools && (scopedAllowRules.length || !allowedTools.length)) {
    options.allowedTools = scopedAllowRules
  }
  // Pin to a specific claude binary when the broker provides one; otherwise the
  // SDK uses its bundled CLI (both read the same macOS Keychain login).
  if (process.env.DEVDY_CLAUDE_PATH) options.pathToClaudeCodeExecutable = process.env.DEVDY_CLAUDE_PATH
  // GĐ6 (AC1): per-project git account context. Keep the default Claude Code
  // preset system prompt and APPEND our context (a plain string would replace
  // the whole prompt and lose tool guidance).
  // Resume fallback: a resumed first prompt carries no options, so the broker
  // passes the conductor framing via DEVDY_APPEND_SYSTEM_PROMPT (the SDK does not
  // persist an appended system prompt across a resume).
  const appendSystemPrompt = opts.appendSystemPrompt || SIDECAR_ENV.DEVDY_APPEND_SYSTEM_PROMPT
  if (appendSystemPrompt) {
    options.systemPrompt = {
      type: 'preset',
      preset: 'claude_code',
      append: String(appendSystemPrompt),
    }
  }
  if (opts.noTools === true) applyNoTools(options)
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
    let failed = false
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
      failed = true
      send({ type: '_devdy_error', error: String(err && err.stack ? err.stack : err) })
    } finally {
      stopUsagePolling()
      await flushUsageCaptures()
      // `code` mirrors the process exit code: non-zero = the session failed.
      send({ type: '_devdy_closed', code: failed ? 1 : 0 })
      exitAfterSession(failed ? 1 : 0)
    }
  })()
}

// The session is over (finished, or the CLI died): nothing more can happen in
// this process, and a follow-up prompt pushed into the dead queue would vanish
// silently. Stop reading stdin so Node exits once stdout drains; the broker sees
// EOF and finishes the run (a follow-up then resumes in a fresh sidecar). The
// unref'd timer only fires if some stray handle still holds the loop open.
function exitAfterSession(code = 0) {
  process.exitCode = code
  rl.close()
  process.stdin.destroy()
  setTimeout(() => process.exit(code), 2000).unref()
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

// Validate one model id by running the shortest possible real turn through the
// SAME SDK path a run uses — so the binary the SDK resolves (its bundled CLI, or
// DEVDY_CLAUDE_PATH when set) is exactly the one a real run would use. A direct
// `claude --model` spawn could hit a different binary and give a false verdict.
// Emits `_devdy_valid` on a successful result, `_devdy_invalid` (with a reason)
// when the turn errors or the model is rejected.
function startValidateModel(opts = {}) {
  inputQueue = new MessageQueue()
  inputQueue.push(userMessage('say ok'))

  // The bundled CLI does NOT hard-fail an unknown model — it warns on stderr and
  // silently falls back to a default, then completes a "successful" turn. So a
  // green result is not enough: an unrecognized id must be caught from this
  // warning, or we would green-light a model the run would never actually use.
  let stderrAll = ''
  // The SDK/stream path flags an unknown id with a structured stderr tag —
  // `[claude-code:unrecognized_model] {"model":"…","query_source":"sdk"}` — while
  // the print CLI uses a prose sentence. Match either so a fallback is caught.
  const UNKNOWN_MODEL_HINTS = ['unrecognized_model', 'is not a model this version of claude code recognizes']
  const options = {
    permissionMode: 'bypassPermissions',
    includePartialMessages: false,
    stderr: (data) => {
      const text = String(data || '')
      if (text.trim()) stderrAll += text
    },
  }
  if (opts.model) options.model = opts.model
  if (opts.cwd) options.cwd = opts.cwd
  if (process.env.DEVDY_CLAUDE_PATH) options.pathToClaudeCodeExecutable = process.env.DEVDY_CLAUDE_PATH

  const firstStderrLine = () => stderrAll.split('\n').map((l) => l.trim()).find(Boolean) || ''

  ;(async () => {
    try {
      currentQuery = query({ prompt: inputQueue, options })
      let completed = false
      let failure = ''
      for await (const message of currentQuery) {
        if (message && message.type === 'result') {
          if (message.subtype === 'success') completed = true
          else failure = message.subtype || 'result_error'
          break
        }
      }
      if (inputQueue) inputQueue.close()
      // A turn can complete on a fallback even though the id was never honored.
      const lowered = stderrAll.toLowerCase()
      const unrecognized = UNKNOWN_MODEL_HINTS.some((h) => lowered.includes(h))
      if (completed && !unrecognized) {
        send({ type: '_devdy_valid' })
      } else if (unrecognized) {
        send({ type: '_devdy_invalid', error: 'Not recognized by this Claude Code version' })
      } else {
        send({ type: '_devdy_invalid', error: failure || firstStderrLine() || 'No successful result' })
      }
    } catch (err) {
      // A model the CLI rejects outright throws here; the reason is usually in
      // the error message (and sometimes only on stderr), so prefer either.
      const msg = String(err && err.message ? err.message : err).trim()
      send({ type: '_devdy_invalid', error: msg || firstStderrLine() || 'Validation failed' })
    } finally {
      if (inputQueue) inputQueue.close()
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
    case 'validate_model':
      if (!started) {
        started = true
        startValidateModel(cmd.options || {})
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
  // stdin closed by the broker → end the session input. The SDK keeps the CLI
  // running until its turn ends, so a tool may still ask for permission — but no
  // answer can reach us any more: settle every open request now (and deny new
  // ones in canUseTool) instead of leaving the turn blocked forever.
  inputClosed = true
  if (inputQueue) inputQueue.close()
  for (const requestId of [...pendingPermissions.keys()]) {
    cancelPermission(requestId, INPUT_CLOSED_REASON, 'warn', '⚠ An open permission request was denied: the session can no longer receive answers.')
  }
})

send({ type: '_devdy_ready', capabilities: { heartbeat_ms: HEARTBEAT_MS } })
