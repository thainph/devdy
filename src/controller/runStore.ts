/**
 * Turns decrypted {@link StreamPayload}s into the `StreamEntry[]` that the
 * reused {@link StreamLog} component renders (FR-004/005), plus the pending
 * permission request the {@link PermissionPrompt} shows (FR-006).
 *
 * History vs live (FR-005/AC-07): the Host replays the full persisted log in
 * `history` batches, then live events continue. The Controller only started
 * receiving live frames *after* it joined, so there is no overlap — history is
 * rebuilt from the batched raw NDJSON lines and live entries are appended after
 * it. A reconnect re-requests history; we reset the run's entries and rebuild,
 * so nothing is duplicated (FR-012/AC-15).
 */
import { reactive } from 'vue'
import {
  applyStreamEvent,
  extractContextTokens,
  extractTurnTotalTokens,
  isCompactBoundary,
  parseStreamLog,
  type ImageAttachment,
  type StreamEntry,
  type StreamState,
} from '@/lib/streamEvents'
import type { RateLimitWindows } from '@/stores/liveRuns'
import type { PermissionRequest } from '@/components/PermissionPrompt.vue'
import type {
  EngineModelOption,
  PlanBudget,
  ProjectInfo,
  RunInfo,
  SlashCommand,
  StreamPayload,
} from './protocol'

/** The bound run's per-turn usage badge (tokens + cost), from a `result` event. */
export interface UsageInfo {
  totalTokens?: number
  costUsd?: number
  costEstimated?: boolean
}

/** Live + replayed state for a single run the Controller is viewing. */
interface RunView {
  runId: string
  /** Rendered entries (history first, then live) for StreamLog. */
  entries: StreamEntry[]
  toolIndex: Map<string, number>
  /** Accumulating raw history NDJSON lines until the final batch arrives. */
  historyBuffer: string[]
  /** True once history has been received & folded in (so live can append). */
  historyDone: boolean
  status: string | null
  /** Context-window occupancy (tokens), mirrors the desktop ContextMeter. */
  contextTokens: number
  /** Model id from `system.init` — resolves the context-window limit. */
  model: string | null
  /** True once a per-message `assistant` usage set contextTokens this turn. */
  sawAssistantUsage: boolean
  /** Real claude.ai subscription rate-limit windows, if reported. */
  rateLimit: RateLimitWindows | null
  /** Latest turn's usage (tokens + cost) for the usage badge. */
  usage: UsageInfo | null
}

/** Coerce a claude.ai `resets_at` (ISO string or epoch secs/ms) to ISO. */
function toIso(v: unknown): string | null {
  if (typeof v === 'string') return v
  if (typeof v === 'number') return new Date(v > 1e12 ? v : v * 1000).toISOString()
  return null
}

/** Fold claude.ai rate-limit windows from a `system.init` / `rate_limit_event`
 * (mirror of `liveRuns.captureRateLimit`). Claude subscription only. */
function captureRateLimit(run: RunView, p: Record<string, unknown> | null): void {
  if (!p) return
  if (p.type === 'system' && p.subtype === 'init' && p.rate_limits && typeof p.rate_limits === 'object') {
    const rl = p.rate_limits as Record<string, unknown>
    const win = (k: string): RateLimitWindows['fiveHour'] | undefined => {
      const w = rl[k] as Record<string, unknown> | null | undefined
      if (!w) return undefined
      return {
        utilization: typeof w.utilization === 'number' ? w.utilization : null,
        resetsAt: toIso(w.resets_at),
      }
    }
    run.rateLimit = { fiveHour: win('five_hour'), sevenDay: win('seven_day') }
  } else if (p.type === 'rate_limit_event' && p.rate_limit_info && typeof p.rate_limit_info === 'object') {
    const info = p.rate_limit_info as Record<string, unknown>
    const win = {
      utilization: typeof info.utilization === 'number' ? info.utilization : null,
      resetsAt: toIso(info.resetsAt),
    }
    const next: RateLimitWindows = { ...(run.rateLimit ?? {}) }
    if (info.rateLimitType === 'five_hour') next.fiveHour = win
    else if (typeof info.rateLimitType === 'string' && info.rateLimitType.startsWith('seven_day'))
      next.sevenDay = win
    run.rateLimit = next
  }
}

/** Fold usage/context metrics from one stream event (mirror of the desktop
 * liveRuns pipeline). Must run AFTER `applyStreamEvent` so a `result` entry is
 * already in `run.entries`. */
function foldMetrics(run: RunView, event: unknown): void {
  const p = event && typeof event === 'object' ? (event as Record<string, unknown>) : null
  if (p) {
    if (p.type === 'system' && p.subtype === 'init' && typeof p.model === 'string') run.model = p.model
    captureRateLimit(run, p)
  }
  // Context-window occupancy: use the LATEST per-message assistant usage (the
  // newest message carries the full history as cache_read, so it is the true
  // current window size — mirrors Claude's own context indicator). Fall back to
  // the result cumulative total; reset on a compaction boundary.
  if (isCompactBoundary(event)) {
    run.contextTokens = 0
    run.sawAssistantUsage = false
  } else {
    const ctx = extractContextTokens(event)
    if (ctx !== null) {
      run.contextTokens = ctx
      run.sawAssistantUsage = true
    } else if (!run.sawAssistantUsage) {
      const total = extractTurnTotalTokens(event)
      if (total !== null) run.contextTokens = total
    }
  }
  // Usage badge from the just-folded `result` entry.
  const last = run.entries[run.entries.length - 1]
  if (last && last.kind === 'result') {
    run.usage = {
      totalTokens: last.total_tokens,
      costUsd: last.cost_usd,
      costEstimated: last.cost_estimated,
    }
  }
}

/** The reactive facade the UI binds to. */
export interface RunStoreState {
  /** Ordered run ids seen (newest activity last). */
  runIds: string[]
  /** Per-run views keyed by run id. */
  runs: Record<string, RunView>
  /** The run currently shown in the UI, or null (null → show the run browser). */
  activeRunId: string | null
  /** All runs the Host reports (legacy; unused in the single-run controller). */
  runList: RunInfo[]
  /** Projects (legacy; unused in the single-run controller). */
  projectList: ProjectInfo[]
  /** The single pending permission request (FR-006), or null. */
  pending: PermissionRequest | null
  /** Transient advisory from the Host (e.g. "already handled"). */
  notice: string | null
  /** Slash commands the Host advertises for the bound run (composer palette). */
  slashCommands: SlashCommand[]
  /** Engine/model options the Host offers (composer footer selectors). */
  engineModelOptions: EngineModelOption[]
  /** Project file paths for the bound run (`@`-mention autocomplete). */
  projectFiles: string[]
  /** The bound run's live selection, mirrored from the Host (engine/model/mode). */
  runMeta: { engine: string; model: string; permissionMode: string }
  /** Subscription plan-usage badges, mirrored from the desktop `BudgetBadge`. */
  claudeBudget: PlanBudget | null
  codexBudget: PlanBudget | null
}

/**
 * A tiny reactive store. Not Pinia (the controller app is deliberately minimal
 * and standalone) — a `reactive` object is enough and keeps the bundle small.
 */
export function createRunStore() {
  const state = reactive<RunStoreState>({
    runIds: [],
    runs: {},
    activeRunId: null,
    runList: [],
    projectList: [],
    pending: null,
    notice: null,
    slashCommands: [],
    engineModelOptions: [],
    projectFiles: [],
    runMeta: { engine: '', model: '', permissionMode: '' },
    claudeBudget: null,
    codexBudget: null,
  })

  function ensureRun(runId: string): RunView {
    let run = state.runs[runId]
    if (!run) {
      run = {
        runId,
        entries: [],
        toolIndex: new Map(),
        historyBuffer: [],
        historyDone: false,
        status: null,
        contextTokens: 0,
        model: null,
        sawAssistantUsage: false,
        rateLimit: null,
        usage: null,
      }
      state.runs[runId] = run
      state.runIds.push(runId)
    }
    // NOTE: we do NOT auto-select the active run here. The user browses the run
    // list and explicitly opens a run — a live event from any run must not hijack
    // whatever the user is currently viewing.
    return run
  }

  /** Fold a decrypted payload into the store. */
  function apply(payload: StreamPayload): void {
    switch (payload.kind) {
      case 'history':
        applyHistory(payload.run_id, payload.lines, payload.done)
        break
      case 'stream':
        ensureRun(payload.run_id).status = 'running'
        applyLiveEvent(payload.run_id, payload.event)
        break
      case 'output':
        ensureRun(payload.run_id).status = 'running'
        applyOutput(payload.run_id, payload.line, payload.is_stderr === true)
        break
      case 'permission_request':
        state.pending = {
          run_id: payload.run_id,
          request_id: payload.request_id,
          tool_name: payload.tool,
          tool_input: payload.input,
          cwd: payload.cwd ?? null,
        }
        ensureRun(payload.run_id).status = 'running'
        // Jump to the run that needs input (desktop parity — a permission is
        // the one event worth pulling the user's attention to).
        state.activeRunId = payload.run_id
        break
      case 'done':
        ensureRun(payload.run_id).status = payload.status
        // Reflect the new status in the browse list too, if present.
        {
          const info = state.runList.find((r) => r.id === payload.run_id)
          if (info) info.status = payload.status
        }
        break
      case 'slash_command_list':
        state.slashCommands = payload.commands
        break
      case 'engine_model_options':
        state.engineModelOptions = payload.engines
        break
      case 'project_file_list':
        state.projectFiles = payload.files
        break
      case 'run_meta':
        // Mirror the Host's live selection. Only overwrite a field the Host
        // actually sent (null = "no opinion") so a partial push can't blank the
        // others.
        if (payload.engine != null) state.runMeta.engine = payload.engine
        if (payload.model != null) state.runMeta.model = payload.model
        if (payload.permission_mode != null) state.runMeta.permissionMode = payload.permission_mode
        break
      case 'plan_usage':
        // Mirror the desktop BudgetBadge (Claude + Codex). A provider may be null
        // or `source==='disabled'` when no plan snapshot exists yet.
        state.claudeBudget = payload.claude ?? null
        state.codexBudget = payload.codex ?? null
        break
      case 'run_list':
        // Legacy: unused in the single-run controller (kept so the Host push,
        // if any, doesn't fall through to the decode-error counter upstream).
        state.runList = payload.runs
        for (const info of payload.runs) {
          ensureRun(info.id).status = info.status
        }
        break
      case 'project_list':
        state.projectList = payload.projects
        break
      case 'session_token':
      case 'auth_result':
        // Transport-level auth signals — handled in `connection.ts`; ignore here.
        break
      case 'notice':
        state.notice = payload.text
        // A "notice" often means a permission was resolved elsewhere; clear the
        // stale prompt so the Controller isn't stuck on a handled request.
        if (/already handled/i.test(payload.text)) {
          state.pending = null
        }
        break
      case 'permission_resolved':
        if (
          state.pending?.run_id === payload.run_id &&
          state.pending?.request_id === payload.request_id
        ) {
          // Accepted means the Host delivered the decision. "already_handled"
          // also resolves this exact stale prompt. A delivery failure stays
          // visible so the user can retry.
          if (payload.accepted || payload.reason === 'already_handled') {
            state.pending = null
          }
        }
        if (!payload.accepted && payload.reason === 'delivery_failed') {
          state.notice = 'Host chưa nhận được phản hồi permission — hãy thử lại.'
        }
        break
      case 'command_ack':
        // Transport correlation is consumed by connection.ts.
        break
    }
  }

  function applyHistory(runId: string, lines: string[], done: boolean): void {
    const run = ensureRun(runId)
    // A fresh history stream (first batch after a (re)connect) resets the run so
    // a reconnect rebuilds cleanly without duplicating entries.
    if (run.historyDone) {
      run.entries = []
      run.toolIndex = new Map()
      run.historyBuffer = []
      run.historyDone = false
    }
    run.historyBuffer.push(...lines)
    if (!done) return
    const buffered = run.historyBuffer
    const parsed: StreamState | null = parseStreamLog(buffered.join('\n'))
    if (parsed) {
      run.entries = parsed.entries
      run.toolIndex = parsed.toolIndex
      // Context occupancy + model come folded from the parser.
      run.contextTokens = parsed.contextTokens
      run.model = parsed.model
      run.sawAssistantUsage = parsed.contextTokens > 0
      // Usage badge = the last result entry in history.
      const lastResult = [...parsed.entries].reverse().find((e) => e.kind === 'result')
      if (lastResult && lastResult.kind === 'result') {
        run.usage = {
          totalTokens: lastResult.total_tokens,
          costUsd: lastResult.cost_usd,
          costEstimated: lastResult.cost_estimated,
        }
      }
      // Rate-limit windows aren't captured by parseStreamLog — scan raw lines.
      for (const line of buffered) {
        const t = line.trim()
        if (!t.startsWith('{')) continue
        try {
          captureRateLimit(run, JSON.parse(t) as Record<string, unknown>)
        } catch {
          // ignore malformed history line
        }
      }
    }
    run.historyBuffer = []
    run.historyDone = true
  }

  function applyLiveEvent(runId: string, event: unknown): void {
    const run = ensureRun(runId)
    applyStreamEvent(run, event)
    foldMetrics(run, event)
    // Do not steal focus: live events fold into their run's view silently; the
    // user stays on whatever they opened.
  }

  function applyOutput(runId: string, line: string, isStderr: boolean): void {
    const run = ensureRun(runId)
    // A raw output line may itself be a JSON SDK event (NDJSON) — fold it in as
    // a stream event when so; otherwise show it as a console log/error entry.
    const trimmed = line.trim()
    if (trimmed.startsWith('{')) {
      try {
        applyStreamEvent(run, JSON.parse(trimmed))
        return
      } catch {
        // not JSON — fall through to a plain line
      }
    }
    run.entries.push(
      isStderr ? { kind: 'error', text: line } : { kind: 'log', level: 'info', text: line },
    )
  }

  /** Open a run (from the browse list or the seen set). Creates its view if the
   * run has not streamed anything yet, so history can be requested into it. */
  function setActiveRun(runId: string): void {
    ensureRun(runId)
    state.activeRunId = runId
  }

  /** Return to the run browser (home). */
  function clearActiveRun(): void {
    state.activeRunId = null
  }

  /** Optimistically echo the user's own turn into the timeline. The live SDK
   * stream does NOT replay our prompt, so — like the desktop `live.pushUser` —
   * the controller must render it itself. A later history rebuild resets and
   * replaces the entries (from the transcript), so this never duplicates. */
  function pushUser(runId: string, text: string, images?: ImageAttachment[]): void {
    const run = ensureRun(runId)
    const t = text.trim()
    if (!t && !(images && images.length)) return
    run.entries.push({ kind: 'user', text: t, ...(images && images.length ? { images } : {}) })
    run.status = 'running'
  }

  function clearPending(): void {
    state.pending = null
  }

  function clearNotice(): void {
    state.notice = null
  }

  /** Drop everything (used when the connection is torn down / re-paired). */
  function reset(): void {
    state.runIds = []
    state.runs = {}
    state.activeRunId = null
    state.runList = []
    state.projectList = []
    state.pending = null
    state.notice = null
    state.slashCommands = []
    state.engineModelOptions = []
    state.projectFiles = []
    state.runMeta = { engine: '', model: '', permissionMode: '' }
  }

  return { state, apply, pushUser, setActiveRun, clearActiveRun, clearPending, clearNotice, reset }
}

export type RunStore = ReturnType<typeof createRunStore>
