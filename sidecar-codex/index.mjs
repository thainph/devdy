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

// Forward a codex RateLimitSnapshot to the broker as the same `_devdy_usage`
// control message the Claude sidecar emits, tagged `provider:"codex"`. The Rust
// drain persists it as the latest Codex plan-usage state (the equivalent of
// Claude's `/usage`). Best-effort — never throws into the RPC loop.
function emitCodexRateLimits(snapshot) {
  if (!snapshot || typeof snapshot !== 'object') return
  const rl = snapshot.rateLimits || snapshot.rateLimitsByLimitId?.codex || snapshot
  if (!rl || typeof rl !== 'object') return
  if (!rl.primary && !rl.secondary) return
  out({ type: '_devdy_usage', usage: { provider: 'codex', rate_limits: rl } })
}

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

// ── centrally-managed MCP servers → codex `-c mcp_servers.*` overrides ─────
// DEVDY_CODEX_MCP is the resolved Record<name, config>. Rust forwards stdio and
// streamable HTTP servers to Codex. We turn each into TOML config override flags
// placed BEFORE 'app-server'.

// Escape a string as a TOML basic string: wrap in double quotes, escape
// backslash and double-quote (and control chars codex would choke on).
function tomlString(s) {
  const escaped = String(s)
    .replace(/\\/g, '\\\\')
    .replace(/"/g, '\\"')
    .replace(/\n/g, '\\n')
    .replace(/\r/g, '\\r')
    .replace(/\t/g, '\\t')
  return `"${escaped}"`
}

function tomlKey(s) {
  return tomlString(s)
}

// TOML array of strings: ["a","b"]
function tomlStringArray(arr) {
  return `[${arr.map(tomlString).join(',')}]`
}

// TOML inline table of string values: {KEY="val",KEY2="val2"}
function tomlInlineTable(obj) {
  const parts = Object.entries(obj).map(([k, v]) => `${tomlKey(k)}=${tomlString(v)}`)
  return `{${parts.join(',')}}`
}

function envNamePart(s) {
  const cleaned = String(s).replace(/[^A-Za-z0-9_]/g, '_').replace(/^_+|_+$/g, '')
  return (cleaned || 'SERVER').toUpperCase()
}

function buildMcpConfig() {
  const raw = process.env.DEVDY_CODEX_MCP
  const env = { ...process.env }
  // Keep the resolved secret bundle in the Node sidecar only; Codex receives the
  // specific config/env it needs below.
  delete env.DEVDY_CODEX_MCP
  if (!raw) return { args: [], env }
  let servers
  try {
    servers = JSON.parse(raw)
  } catch {
    return { args: [], env }
  }
  if (!servers || typeof servers !== 'object') return { args: [], env }
  const args = []
  let serverIndex = 0
  for (const [name, cfg] of Object.entries(servers)) {
    const currentServerIndex = serverIndex++
    if (!cfg || typeof cfg !== 'object') continue
    if (cfg.type === 'stdio' && cfg.command) {
      args.push('-c', `mcp_servers.${name}.command=${tomlString(cfg.command)}`)
      if (Array.isArray(cfg.args) && cfg.args.length) {
        args.push('-c', `mcp_servers.${name}.args=${tomlStringArray(cfg.args)}`)
      }
      if (cfg.env && typeof cfg.env === 'object' && Object.keys(cfg.env).length) {
        args.push('-c', `mcp_servers.${name}.env=${tomlInlineTable(cfg.env)}`)
      }
      continue
    }

    if ((cfg.type === 'http' || cfg.type === 'streamable_http') && cfg.url) {
      args.push('-c', `mcp_servers.${name}.url=${tomlString(cfg.url)}`)
      if (cfg.headers && typeof cfg.headers === 'object' && Object.keys(cfg.headers).length) {
        const headerEnvRefs = {}
        let i = 0
        for (const [header, value] of Object.entries(cfg.headers)) {
          const envName = `DEVDY_MCP_HTTP_${envNamePart(name)}_${currentServerIndex}_${i++}`
          env[envName] = String(value)
          headerEnvRefs[header] = envName
        }
        args.push('-c', `mcp_servers.${name}.env_http_headers=${tomlInlineTable(headerEnvRefs)}`)
      }
    }
  }
  return { args, env }
}

// ── codex app-server JSON-RPC plumbing ────────────────────────────────────
const mcpConfig = buildMcpConfig()
const codex = spawn(CODEX_BIN, [...mcpConfig.args, 'app-server'], { cwd: CWD, stdio: ['pipe', 'pipe', 'pipe'], env: mcpConfig.env })
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
const permWaiters = new Map() // requestId → { rpcId, command, kind, ... }
// Commands the user denied — so the matching commandExecution item renders as
// rejected (not as a succeeded run).
const declinedCommands = new Set()
const cmdKey = (c) => (Array.isArray(c) ? c.join(' ') : String(c ?? '')).trim()

function emitPermission(rpcReqId, { tool_name, tool_input, title, description, display_name }, waiter = {}) {
  const requestId = `codex-${rpcReqId}`
  permWaiters.set(requestId, { rpcId: rpcReqId, command: tool_input?.command, kind: 'approval', ...waiter })
  ctrl('_devdy_permission_request', { requestId, tool_name, tool_input, title, description, display_name })
}

function enumOptions(schema) {
  if (Array.isArray(schema?.enum)) {
    return schema.enum.map((v) => ({ label: String(v), description: '' }))
  }
  if (Array.isArray(schema?.oneOf)) {
    return schema.oneOf.map((opt) => ({
      label: String(opt?.const ?? opt?.title ?? ''),
      description: String(opt?.description ?? opt?.title ?? ''),
    })).filter((opt) => opt.label)
  }
  return []
}

function elicitationQuestions(params) {
  const schema = params?.requestedSchema
  const props = schema?.properties && typeof schema.properties === 'object' ? schema.properties : {}
  const required = new Set(Array.isArray(schema?.required) ? schema.required : [])
  const questions = []
  const answerKeys = {}

  for (const [key, prop] of Object.entries(props)) {
    if (!prop || typeof prop !== 'object') continue
    const title = String(prop.title || key)
    let question = title
    if (answerKeys[question]) question = `${title} (${key})`
    const options = enumOptions(prop)
    if (prop.type === 'boolean' && !options.length) {
      options.push(
        { label: 'true', description: 'Yes' },
        { label: 'false', description: 'No' },
      )
    }
    questions.push({
      header: `${params?.serverName || 'MCP'}${required.has(key) ? ' · required' : ''}`,
      question,
      multiSelect: prop.type === 'array',
      options,
    })
    answerKeys[question] = { key, type: prop.type }
  }
  return { questions, answerKeys }
}

function coerceElicitationAnswer(value, type) {
  if (type === 'boolean') return String(value).trim().toLowerCase() === 'true'
  if (type === 'number' || type === 'integer') {
    const n = Number(value)
    return Number.isFinite(n) ? n : value
  }
  if (type === 'array') {
    return String(value).split(',').map((v) => v.trim()).filter(Boolean)
  }
  return value
}

function elicitationContentFromAnswers(answers, answerKeys) {
  const content = {}
  for (const [question, value] of Object.entries(answers || {})) {
    const meta = answerKeys?.[question]
    if (!meta) continue
    content[meta.key] = coerceElicitationAnswer(value, meta.type)
  }
  return content
}

function pickString(obj, keys) {
  for (const key of keys) {
    const value = obj?.[key]
    if (typeof value === 'string' && value.trim()) return value.trim()
  }
  return ''
}

function mcpToolUseId(rpcReqId) {
  return `mcp_${String(rpcReqId).replace(/[^A-Za-z0-9_-]/g, '_')}`
}

function mcpDisplayName(params, fallback) {
  return pickString(params, ['displayName', 'toolDisplayName', 'toolTitle', 'title', 'toolName', 'name']) || fallback
}

function mcpServerDisplayName(params, server) {
  return pickString(params, ['serverDisplayName', 'serverTitle']) || server
}

function mcpTranscriptInput(params, server, questions = []) {
  const schema = params?.requestedSchema
  const fields = schema?.properties && typeof schema.properties === 'object'
    ? Object.keys(schema.properties)
    : []
  return {
    serverName: server,
    mode: params?.mode,
    message: params?.message,
    url: params?.url,
    toolName: pickString(params, ['toolName', 'name']) || undefined,
    ...(fields.length ? { fields } : {}),
    ...(questions.length
      ? {
          questions: questions.map((q) => ({
            header: q.header,
            question: q.question,
            options: q.options.map((o) => o.label),
          })),
        }
      : {}),
  }
}

function emitMcpToolUse(rpcReqId, params, { displayName, input }) {
  const id = mcpToolUseId(rpcReqId)
  const server = input.serverName || params?.serverName || 'MCP server'
  out({
    type: 'assistant',
    message: {
      role: 'assistant',
      content: [{ type: 'tool_use', id, name: 'MCP', input }],
    },
    tool_use_meta: [{
      id,
      display_name: displayName,
      server_display_name: mcpServerDisplayName(params, server),
    }],
  })
  return id
}

function emitMcpToolResult(toolUseId, accepted) {
  if (!toolUseId) return
  out({
    type: 'user',
    message: {
      role: 'user',
      content: [{
        type: 'tool_result',
        tool_use_id: toolUseId,
        content: accepted ? 'Accepted by user.' : 'Denied by user.',
        is_error: !accepted,
      }],
    },
  })
}

function emitMcpElicitation(rpcReqId, params) {
  const server = params?.serverName || 'MCP server'
  if (params?.mode === 'form') {
    const { questions, answerKeys } = elicitationQuestions(params)
    if (questions.length) {
      const toolUseId = emitMcpToolUse(rpcReqId, params, {
        displayName: mcpDisplayName(params, 'MCP input'),
        input: mcpTranscriptInput(params, server, questions),
      })
      emitPermission(rpcReqId, {
        tool_name: 'AskUserQuestion',
        tool_input: { questions, mcp: { serverName: server, message: params.message, mode: params.mode } },
        title: `${server} requests input`,
        description: params.message,
        display_name: 'MCP input',
      }, { kind: 'mcp_elicitation_form', answerKeys, toolUseId })
      return
    }
  }

  const toolUseId = emitMcpToolUse(rpcReqId, params, {
    displayName: mcpDisplayName(params, 'MCP request'),
    input: mcpTranscriptInput(params, server),
  })
  emitPermission(rpcReqId, {
    tool_name: 'MCP',
    tool_input: {
      serverName: server,
      mode: params?.mode,
      message: params?.message,
      url: params?.url,
    },
    title: `${server} requests permission`,
    description: params?.message || 'Allow this MCP request to continue?',
    display_name: 'MCP request',
  }, { kind: 'mcp_elicitation', toolUseId })
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
      case 'mcpServer/elicitation/request':
        if (process.env.DEVDY_CODEX_MCP_TRACE) {
          note(`[codex] MCP elicitation from ${p.serverName ?? 'server'}: ${p.mode ?? 'unknown'}`, 'debug')
        }
        emitMcpElicitation(msg.id, p)
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
      // Live plan rate-limit updates (5h + weekly windows). Forwarded so the
      // budget badge / settings panel refresh mid-run, like Claude's /usage.
      case 'account/rateLimits/updated':
        emitCodexRateLimits(p)
        break
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

  // Snapshot plan usage right away (cheap, no turn) so the badge is fresh from
  // the start of the run. Fire-and-forget; failures are non-fatal.
  request('account/rateLimits/read', undefined)
    .then((r) => emitCodexRateLimits(r?.result))
    .catch(() => {})

  ready = true
  for (const t of promptQueue.splice(0)) startTurn(t.text, t.images)
}

// Lightweight usage probe: initialize the app-server, read the account's plan
// rate-limits WITHOUT starting a thread or running a turn (no quota spend), emit
// them, then exit. Mirrors the Claude sidecar's `usage_probe`. Triggered by the
// Rust `refresh_codex_plan_usage` command via DEVDY_CODEX_USAGE_PROBE=1.
async function usageProbe() {
  try {
    await request('initialize', {
      clientInfo: { name: 'devdy-codex', title: 'Devdy', version: '0.1.0' },
      capabilities: { experimentalApi: true, requestAttestation: false },
    })
    rpcNotify('initialized', undefined)
    const r = await request('account/rateLimits/read', undefined)
    emitCodexRateLimits(r?.result)
    ctrl('_devdy_done')
  } catch (e) {
    ctrl('_devdy_error', { error: String(e?.stack || e) })
  } finally {
    shutdown(0)
  }
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
      if (w.kind === 'mcp_elicitation_form') {
        const content = accept ? elicitationContentFromAnswers(cmd.answers, w.answerKeys) : null
        emitMcpToolResult(w.toolUseId, accept)
        rpcRespond(w.rpcId, { action: accept ? 'accept' : 'decline', content, _meta: null })
        break
      }
      if (w.kind === 'mcp_elicitation') {
        emitMcpToolResult(w.toolUseId, accept)
        rpcRespond(w.rpcId, { action: accept ? 'accept' : 'decline', content: null, _meta: null })
        break
      }
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

if (process.env.DEVDY_CODEX_USAGE_PROBE === '1') {
  usageProbe()
} else {
  bringUp().catch((e) => { ctrl('_devdy_error', { error: String(e?.stack || e) }); shutdown(1) })
}
