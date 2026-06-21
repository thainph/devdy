// Devdy Codex sidecar.
//
// Drives `codex app-server` (JSON-RPC over stdio) and bridges it to the Devdy
// Rust broker using the SAME protocol as the Claude sidecar, so `drain_sidecar`,
// the frontend stream renderer, and the permission modal all work UNCHANGED:
//
//   stdout (to broker):
//     - Claude-shaped stream-json messages (translated from codex notifications):
//         {type:"system",subtype:"init",session_id,model,cwd,tools}
//         {type:"assistant",message:{content:[{type:"text"|"thinking"|"tool_use",...}]}}
//         {type:"user",message:{content:[{type:"tool_result",tool_use_id,content}]}}
//         {type:"result",subtype:"success"}            ← ends the turn
//     - control, namespaced `_devdy_*`:
//         {type:"_devdy_ready"} / {type:"_devdy_done"} / {type:"_devdy_error",error}
//         {type:"_devdy_permission_request",requestId,tool_name,tool_input,title,description}
//
//   stdin (from broker): one JSON command per line
//     {type:"prompt",text,options?}            → start a turn
//     {type:"permission_response",requestId,decision}  ("allow" | "deny")
//     {type:"interrupt"} / {type:"abort"}
//
// Auth: `codex app-server` reads the same ChatGPT login as the codex CLI, so
// runs bill against the user's subscription — no API key.

import { spawn } from 'node:child_process'

const CODEX_BIN = process.env.DEVDY_CODEX_PATH || 'codex'
const RESUME_THREAD = process.env.DEVDY_RESUME_SESSION || null
const PERMISSION_MODE = process.env.DEVDY_PERMISSION_MODE || 'default'
const MODEL = process.env.DEVDY_CODEX_MODEL || null
const CWD = process.cwd()

function out(obj) { process.stdout.write(JSON.stringify(obj) + '\n') }
function ctrl(type, extra = {}) { out({ type, ...extra }) }

// Diagnostic log line (level: error|warn|info|debug|trace) — rendered as a muted
// log entry in the UI, NOT as a system error. Use for codex tracing + sidecar notes.
function note(text, level = 'info') { ctrl('_devdy_log', { level, text }) }

// codex app-server writes `tracing` records to stderr: ANSI-colored, timestamp-led,
// e.g. "2026-…Z  ERROR codex_core::tools::router: error=…". Strip the ANSI/timestamp
// and pull out the level so the UI can render it calmly instead of as a red error.
const ANSI_RE = /\x1B\[[0-9;]*m/g
const TRACE_RE = /^\s*\d{4}-\d\d-\d\dT[\d:.]+Z\s+(ERROR|WARN|INFO|DEBUG|TRACE)\s+([\s\S]*)$/
let lastLogLevel = 'info'

function classifyStderrLine(raw) {
  const clean = raw.replace(ANSI_RE, '').replace(/\s+$/, '')
  if (!clean.trim()) return
  // Benign: the user just declined a command approval. codex echoes this as an
  // ERROR in its tool router, but the rejected commandExecution item already
  // renders as ✗ in the UI — drop the redundant internal trace entirely.
  if (/rejected by user/i.test(clean)) return
  const m = clean.match(TRACE_RE)
  if (m) { lastLogLevel = m[1].toLowerCase(); note(m[2].trim(), lastLogLevel) }
  // continuation / non-tracing line → keep the prior record's level (don't spike to error)
  else note(clean.replace(/^\s+/, ''), lastLogLevel)
}

// ── codex app-server JSON-RPC plumbing ────────────────────────────────────
const codex = spawn(CODEX_BIN, ['app-server'], { cwd: CWD, stdio: ['pipe', 'pipe', 'pipe'] })
let rpcId = 1
const pending = new Map() // request id → resolve
function request(method, params) {
  const id = rpcId++
  return new Promise((resolve) => {
    pending.set(id, resolve)
    codex.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n')
  })
}
function rpcNotify(method, params) {
  codex.stdin.write(JSON.stringify({ jsonrpc: '2.0', method, params }) + '\n')
}
function rpcRespond(id, result) {
  codex.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, result }) + '\n')
}
function rpcError(id, message) {
  codex.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, error: { code: -32601, message } }) + '\n')
}

// ── permission mode → codex policy/sandbox ────────────────────────────────
function policyFor(mode) {
  let base
  switch (mode) {
    case 'plan':              base = { approvalPolicy: 'never',      sandbox: 'read-only' }; break
    case 'acceptEdits':       base = { approvalPolicy: 'on-failure', sandbox: 'workspace-write' }; break
    case 'bypassPermissions': base = { approvalPolicy: 'never',      sandbox: 'danger-full-access' }; break
    default:                  base = { approvalPolicy: 'on-request', sandbox: 'workspace-write' }
  }
  // Explicit overrides (tuning knob / tests).
  if (process.env.DEVDY_CODEX_APPROVAL) base.approvalPolicy = process.env.DEVDY_CODEX_APPROVAL
  if (process.env.DEVDY_CODEX_SANDBOX) base.sandbox = process.env.DEVDY_CODEX_SANDBOX
  return base
}

// ── pending permission requests: codex rpc id ↔ devdy requestId ────────────
const permWaiters = new Map() // requestId → { rpcId, command }
// Commands the user denied — so the matching commandExecution item renders as
// rejected (not as a succeeded run).
const declinedCommands = new Set()
const cmdKey = (c) => (Array.isArray(c) ? c.join(' ') : String(c ?? '')).trim()

function emitPermission(rpcReqId, { tool_name, tool_input, title, description }) {
  const requestId = `codex-${rpcReqId}`
  permWaiters.set(requestId, { rpcId: rpcReqId, command: tool_input?.command })
  ctrl('_devdy_permission_request', { requestId, tool_name, tool_input, title, description })
}

// ── translate codex item → Claude-shaped stream-json ──────────────────────
let threadId = RESUME_THREAD
let modelName = null
// Latest TokenUsageBreakdown for the current turn. The codex app-server reports
// usage in a separate `thread/tokenUsage/updated` notification (not on
// `turn/completed`), so we stash the `last` breakdown here and attach it to the
// `result` event when the turn completes.
let lastTokenBreakdown = null

// Map a codex TokenUsageBreakdown onto the Claude `usage` shape that the Rust
// drain reads. codex reports `inputTokens` INCLUDING the cached portion, plus a
// separate `cachedInputTokens`; Claude keeps non-cached input and cache_read
// separate, so we subtract to avoid double-counting. Field names vary between
// codex versions (camelCase since the app-server v2 protocol, snake_case
// before), so probe both. Returns null when nothing usable.
function mapCodexUsage(raw) {
  if (!raw || typeof raw !== 'object') return null
  const u = raw.usage || raw.token_usage || raw.total_token_usage || raw
  const num = (...keys) => {
    for (const k of keys) if (typeof u[k] === 'number') return u[k]
    return 0
  }
  const inputTotal = num('inputTokens', 'input_tokens')
  const cached = num('cachedInputTokens', 'cached_input_tokens', 'cache_read_input_tokens')
  const output = num('outputTokens', 'output_tokens') + num('reasoningOutputTokens', 'reasoning_output_tokens')
  const usage = {
    input_tokens: Math.max(inputTotal - cached, 0),
    output_tokens: output,
    cache_creation_input_tokens: 0,
    cache_read_input_tokens: cached,
  }
  if (!usage.input_tokens && !usage.output_tokens && !usage.cache_read_input_tokens) return null
  return usage
}

function emitItem(item) {
  if (!item || typeof item !== 'object') return
  switch (item.type) {
    case 'agentMessage': {
      const text = String(item.text ?? '')
      if (text.trim()) out({ type: 'assistant', message: { role: 'assistant', content: [{ type: 'text', text }] } })
      break
    }
    case 'reasoning': {
      // summaries are usually omitted; surface any text we do get.
      const parts = []
      for (const s of item.summary ?? []) if (typeof s === 'string') parts.push(s)
      for (const c of item.content ?? []) if (c?.text) parts.push(c.text)
      const text = parts.join('\n').trim()
      if (text) out({ type: 'assistant', message: { role: 'assistant', content: [{ type: 'thinking', thinking: text }] } })
      break
    }
    case 'commandExecution': {
      out({ type: 'assistant', message: { role: 'assistant', content: [
        { type: 'tool_use', id: item.id, name: 'Bash', input: { command: item.command, description: (item.commandActions?.[0]?.name) || undefined } },
      ] } })
      const rejected = declinedCommands.delete(cmdKey(item.command))
      const isError = rejected || (item.exitCode != null && item.exitCode !== 0)
      const output = rejected
        ? 'Rejected by user — command was not run.'
        : (item.aggregatedOutput != null ? String(item.aggregatedOutput) : '')
      out({ type: 'user', message: { role: 'user', content: [
        { type: 'tool_result', tool_use_id: item.id, content: output, is_error: isError },
      ] } })
      break
    }
    case 'fileChange': {
      // Map to an Edit-like tool_use so the frontend diff view can render it.
      out({ type: 'assistant', message: { role: 'assistant', content: [
        { type: 'tool_use', id: item.id, name: 'Edit', input: item },
      ] } })
      break
    }
    // userMessage: skip — the broker already logs the synthetic user entry.
    default: break
  }
}

// ── handle a message from codex app-server ────────────────────────────────
function onServerMessage(msg) {
  // server → client REQUEST (approval / elicitation)
  if (msg.method && msg.id != null && !('result' in msg) && !('error' in msg)) {
    const p = msg.params || {}
    switch (msg.method) {
      case 'item/commandExecution/requestApproval':
        emitPermission(msg.id, { tool_name: 'Bash', tool_input: { command: p.command }, title: p.reason || 'Run a shell command', description: p.command })
        return
      case 'execCommandApproval':
        emitPermission(msg.id, { tool_name: 'Bash', tool_input: { command: Array.isArray(p.command) ? p.command.join(' ') : p.command }, title: p.reason || 'Run a shell command' })
        return
      case 'item/fileChange/requestApproval':
        emitPermission(msg.id, { tool_name: 'Edit', tool_input: p, title: p.reason || 'Apply a file change' })
        return
      case 'applyPatchApproval':
        emitPermission(msg.id, { tool_name: 'Edit', tool_input: p, title: p.reason || 'Apply a patch' })
        return
      // ---- requests we auto-decline with their CORRECT response shape -------
      // (each codex server-request has a distinct response type; a generic
      //  `{decision}` fails to deserialize — that caused the elicitation error.)
      case 'mcpServer/elicitation/request':
        note(`[codex] auto-declined MCP elicitation from ${p.serverName ?? 'server'}`, 'warn')
        rpcRespond(msg.id, { action: 'decline', content: null, _meta: null })
        return
      case 'item/tool/requestUserInput':
        rpcRespond(msg.id, { answers: {} })
        return
      case 'item/permissions/requestApproval':
        // Grant nothing this turn (≈ decline); per-command approvals still flow
        // through item/commandExecution/requestApproval above.
        rpcRespond(msg.id, { permissions: {}, scope: 'turn' })
        return
      // ---- requests we genuinely can't serve → JSON-RPC error (not malformed result) ----
      case 'item/tool/call':
      case 'account/chatgptAuthTokens/refresh':
      case 'attestation/generate':
      default:
        note(`[codex] unsupported server request: ${msg.method}`, 'warn')
        rpcError(msg.id, `unsupported by devdy sidecar: ${msg.method}`)
        return
    }
  }

  // response to one of our RPC requests
  if (msg.id != null && ('result' in msg || 'error' in msg)) {
    const resolve = pending.get(msg.id)
    if (resolve) { pending.delete(msg.id); resolve(msg) }
    return
  }

  // notification (streaming)
  if (msg.method) {
    const p = msg.params || {}
    switch (msg.method) {
      case 'item/completed':
        emitItem(p.item)
        break
      // Usage arrives on its own notification (since the app-server v2 protocol);
      // stash the per-turn (`last`) breakdown to attach at turn/completed.
      case 'thread/tokenUsage/updated': {
        const tu = p.tokenUsage || p
        lastTokenBreakdown = tu?.last || tu?.total || tu
        break
      }
      case 'turn/completed': {
        const result = { type: 'result', subtype: 'success', num_turns: 1 }
        // Newer protocol: usage came via thread/tokenUsage/updated. Older
        // protocol: it was carried on the turn/completed params themselves.
        const usage = mapCodexUsage(lastTokenBreakdown) || mapCodexUsage(p)
        if (usage) result.usage = usage
        if (modelName) result.model = modelName
        if (turnStartedAt != null) result.duration_ms = Date.now() - turnStartedAt
        out(result)
        ctrl('_devdy_done')
        shutdown(0)
        break
      }
      case 'error':
        ctrl('_devdy_error', { error: typeof p === 'string' ? p : JSON.stringify(p) })
        break
      default: break // ignore deltas/status/etc for v1
    }
  }
}

// ── codex stdout framing (newline-delimited JSON) ─────────────────────────
let rbuf = ''
codex.stdout.on('data', (chunk) => {
  rbuf += chunk.toString()
  let nl
  while ((nl = rbuf.indexOf('\n')) >= 0) {
    const line = rbuf.slice(0, nl).trim()
    rbuf = rbuf.slice(nl + 1)
    if (!line) continue
    let msg
    try { msg = JSON.parse(line) } catch { continue }
    try { onServerMessage(msg) } catch (e) { ctrl('_devdy_error', { error: String(e?.stack || e) }) }
  }
})
let estderrBuf = ''
codex.stderr.on('data', (d) => {
  estderrBuf += d.toString()
  let nl
  while ((nl = estderrBuf.indexOf('\n')) >= 0) {
    classifyStderrLine(estderrBuf.slice(0, nl))
    estderrBuf = estderrBuf.slice(nl + 1)
  }
})
codex.on('exit', (code) => { ctrl('_devdy_closed', { code }); process.exit(code ?? 0) })

// ── session bring-up ──────────────────────────────────────────────────────
let ready = false
let turnStartedAt = null
const promptQueue = []

async function bringUp() {
  await request('initialize', {
    clientInfo: { name: 'devdy-codex', title: 'Devdy', version: '0.1.0' },
    capabilities: { experimentalApi: true, requestAttestation: false },
  })
  rpcNotify('initialized', undefined)

  const { approvalPolicy, sandbox } = policyFor(PERMISSION_MODE)
  if (RESUME_THREAD) {
    const r = await request('thread/resume', { threadId: RESUME_THREAD })
    threadId = r.result?.thread?.id || RESUME_THREAD
    modelName = r.result?.model || null
  } else {
    const startParams = { cwd: CWD, approvalPolicy, sandbox }
    if (MODEL) startParams.model = MODEL
    const r = await request('thread/start', startParams)
    threadId = r.result?.thread?.id || null
    modelName = r.result?.model || null
  }

  // system.init lets the broker capture session_id (= threadId) for resume.
  out({ type: 'system', subtype: 'init', session_id: threadId, model: modelName, cwd: CWD, tools: ['Bash', 'Edit'] })
  ctrl('_devdy_ready', { threadId })

  ready = true
  for (const t of promptQueue.splice(0)) startTurn(t.text, t.images)
}

// `images` is an array of { media_type, data(base64) }. Codex app-server takes
// images as an `image` input item whose `url` is a base64 `data:` URL.
async function startTurn(text, images) {
  if (!threadId) { ctrl('_devdy_error', { error: 'no thread' }); return }
  turnStartedAt = Date.now()
  const input = []
  if (text) input.push({ type: 'text', text, text_elements: [] })
  if (Array.isArray(images)) {
    for (const img of images) {
      input.push({ type: 'image', url: `data:${img.media_type};base64,${img.data}` })
    }
  }
  if (!input.length) input.push({ type: 'text', text: '', text_elements: [] })
  // Surface a rejected turn/start (e.g. bad input shape) instead of hanging: the
  // turn outcome otherwise arrives only via the turn/completed notification.
  const res = await request('turn/start', { threadId, input })
  if (res?.error) {
    ctrl('_devdy_error', { error: `turn/start failed: ${res.error.message || JSON.stringify(res.error)}` })
  }
}

// ── stdin from the broker ─────────────────────────────────────────────────
let sbuf = ''
process.stdin.on('data', (chunk) => {
  sbuf += chunk.toString()
  let nl
  while ((nl = sbuf.indexOf('\n')) >= 0) {
    const line = sbuf.slice(0, nl).trim()
    sbuf = sbuf.slice(nl + 1)
    if (!line) continue
    let cmd
    try { cmd = JSON.parse(line) } catch { continue }
    handleCommand(cmd)
  }
})
process.stdin.on('end', () => shutdown(0))

function handleCommand(cmd) {
  switch (cmd.type) {
    case 'prompt':
      if (!ready) promptQueue.push({ text: cmd.text, images: cmd.images })
      else startTurn(cmd.text, cmd.images)
      break
    case 'permission_response': {
      const w = permWaiters.get(cmd.requestId)
      if (!w) return
      permWaiters.delete(cmd.requestId)
      const accept = cmd.decision === 'allow'
      if (!accept && w.command != null) declinedCommands.add(cmdKey(w.command))
      // v2 approvals use accept/decline; legacy uses approved/denied. The
      // app-server accepts the v2 vocabulary for the v2 request methods we use.
      rpcRespond(w.rpcId, { decision: accept ? 'accept' : 'decline' })
      break
    }
    case 'interrupt':
      if (threadId) rpcNotify('turn/interrupt', { threadId }).catch?.(() => {})
      break
    case 'abort':
      shutdown(0)
      break
    default: break
  }
}

function shutdown(code) {
  if (estderrBuf.trim()) { classifyStderrLine(estderrBuf); estderrBuf = '' }
  try { codex.kill() } catch {}
  setTimeout(() => process.exit(code), 50)
}

bringUp().catch((e) => { ctrl('_devdy_error', { error: String(e?.stack || e) }); shutdown(1) })
