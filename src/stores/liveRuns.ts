import { defineStore } from 'pinia'
import { reactive, computed } from 'vue'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import {
  applyStreamEvent,
  extractContextTokens,
  extractTurnTotalTokens,
  isCompactBoundary,
  type StreamEntry,
  type ImageAttachment,
} from '@/lib/streamEvents'
import type { PermissionRequest } from '@/components/PermissionPrompt.vue'
import { useRunsStore } from './runs'
import { useToolPermissionsStore } from './toolPermissions'
import { useRemoteControlStore } from './remoteControl'

/**
 * In-memory streaming state for a single run. Lives in this store (not in
 * RunView) so it — and the Tauri event listeners that feed it — survive
 * navigating away from the run screen and switching between runs. That lets
 * several runs stream concurrently and keep accumulating output while the user
 * is looking at a different run.
 */
export interface LiveSession {
  runId: string
  projectId: string
  entries: StreamEntry[]
  outputLines: { text: string; isStderr: boolean }[]
  hasStreamEvents: boolean
  status: string
  sessionId: string | null
  permissionQueue: PermissionRequest[]
  /**
   * Tool names the user chose to auto-allow. Seeded from the project's
   * persisted "allow always" list and extended as the user grants more.
   */
  allowedTools: string[]
  /**
   * Tool names the user chose to auto-deny. Seeded from the project's
   * persisted "deny always" list and extended as the user denies more.
   */
  deniedTools: string[]
  /** Model id from `system.init` — used to resolve the context-window limit. */
  model: string | null
  /** Estimated tokens occupying the context window after the latest turn. */
  contextTokens: number
  /** True once a per-message `assistant` usage has set contextTokens this run. */
  sawAssistantUsage: boolean
  /** Slash commands advertised by the engine on `system.init` (Claude). */
  slashCommands: string[]
  /** Real claude.ai subscription rate-limit windows, when reported (Claude). */
  rateLimit: RateLimitWindows | null
  /** Set when a turn just tipped the global budget over — the next turn is blocked. */
  budgetBlocked: boolean
}

/** A single rate-limit window: percent used (0-100) and ISO reset time. */
export interface RateLimitWindow {
  utilization: number | null
  resetsAt: string | null
}
export interface RateLimitWindows {
  fiveHour?: RateLimitWindow
  sevenDay?: RateLimitWindow
}

const SLASH_CACHE_KEY = 'devdy.slashCommands'

// Built-in slash commands each engine ships with, used to seed the palette on
// the very first session (before any `system.init` has been observed and
// cached). The live init list — which also includes project/user custom
// commands and skills — replaces this as soon as a run starts.
const BUILTIN_SLASH_COMMANDS: Record<string, string[]> = {
  claude: ['compact', 'clear', 'context', 'init', 'review', 'security-review', 'usage', 'cost', 'help'],
  codex: ['compact', 'clear', 'init', 'review'],
}

function loadSlashCache(): Record<string, string[]> {
  try {
    const raw = localStorage.getItem(SLASH_CACHE_KEY)
    const obj = raw ? JSON.parse(raw) : {}
    return obj && typeof obj === 'object' ? obj : {}
  } catch {
    return {}
  }
}

export const useLiveRunsStore = defineStore('liveRuns', () => {
  // Reactive registry of streaming sessions, keyed by run id.
  const sessions = reactive(new Map<string, LiveSession>())
  // Side tables that don't need to be reactive (and shouldn't be proxied).
  const toolIndexes = new Map<string, Map<string, number>>()
  const unlisteners = new Map<string, UnlistenFn[]>()

  // Slash commands advertised by each engine on `system.init`, cached (and
  // persisted) per engine so a brand-new session — which hasn't produced an
  // init event yet — can still offer the palette from a prior session.
  const slashCommandCache = reactive<Record<string, string[]>>(loadSlashCache())
  function cacheSlashCommands(engine: string | undefined | null, cmds: string[]) {
    if (!engine || !cmds.length) return
    slashCommandCache[engine] = cmds
    try {
      localStorage.setItem(SLASH_CACHE_KEY, JSON.stringify(slashCommandCache))
    } catch {
      /* storage unavailable — in-memory cache still works for this session */
    }
  }
  function cachedSlashCommands(engine: string | undefined | null): string[] {
    if (!engine) return []
    return slashCommandCache[engine] || BUILTIN_SLASH_COMMANDS[engine] || []
  }

  function get(runId: string): LiveSession | undefined {
    return sessions.get(runId)
  }

  function ensure(runId: string, projectId: string): LiveSession {
    let s = sessions.get(runId)
    if (!s) {
      s = reactive({
        runId,
        projectId,
        entries: [] as StreamEntry[],
        outputLines: [] as { text: string; isStderr: boolean }[],
        hasStreamEvents: false,
        status: 'running',
        sessionId: null as string | null,
        permissionQueue: [] as PermissionRequest[],
        allowedTools: [] as string[],
        deniedTools: [] as string[],
        model: null as string | null,
        contextTokens: 0,
        sawAssistantUsage: false,
        slashCommands: [] as string[],
        rateLimit: null as RateLimitWindows | null,
        budgetBlocked: false,
      }) as LiveSession
      // Seed the per-run allow/deny lists with the standing choices the user
      // made for this project in earlier runs, so they apply again here.
      const perms = useToolPermissionsStore()
      s.allowedTools = perms.getAllow(projectId)
      s.deniedTools = perms.getDeny(projectId)
      sessions.set(runId, s)
      toolIndexes.set(runId, new Map())
    }
    return s
  }

  /** Begin a fresh turn: drop any accumulated state and stop old listeners. */
  function reset(runId: string, projectId: string): LiveSession {
    discard(runId)
    return ensure(runId, projectId)
  }

  function pushUser(
    runId: string,
    projectId: string,
    text: string,
    images?: ImageAttachment[],
  ) {
    const s = ensure(runId, projectId)
    s.hasStreamEvents = true
    s.entries.push({ kind: 'user', text, ...(images?.length ? { images } : {}) })
  }

  function setStatus(runId: string, status: string) {
    const s = sessions.get(runId)
    if (s) s.status = status
  }

  function isListening(runId: string): boolean {
    return unlisteners.has(runId)
  }

  /** Normalize an epoch (s or ms) or ISO timestamp into an ISO string. */
  function toIso(v: unknown): string | null {
    if (typeof v === 'string') return v
    if (typeof v === 'number') {
      const ms = v > 1e12 ? v : v * 1000
      return new Date(ms).toISOString()
    }
    return null
  }

  /** Fold claude.ai rate-limit info from system.init or rate_limit_event. */
  function captureRateLimit(s: LiveSession, p: Record<string, unknown> | null) {
    if (!p) return
    if (p.type === 'system' && p.subtype === 'init' && p.rate_limits && typeof p.rate_limits === 'object') {
      const rl = p.rate_limits as Record<string, unknown>
      const win = (k: string): RateLimitWindow | undefined => {
        const w = rl[k] as Record<string, unknown> | null | undefined
        if (!w) return undefined
        return {
          utilization: typeof w.utilization === 'number' ? w.utilization : null,
          resetsAt: toIso(w.resets_at),
        }
      }
      s.rateLimit = { fiveHour: win('five_hour'), sevenDay: win('seven_day') }
    } else if (p.type === 'rate_limit_event' && p.rate_limit_info && typeof p.rate_limit_info === 'object') {
      const info = p.rate_limit_info as Record<string, unknown>
      const win: RateLimitWindow = {
        utilization: typeof info.utilization === 'number' ? info.utilization : null,
        resetsAt: toIso(info.resetsAt),
      }
      const next: RateLimitWindows = { ...(s.rateLimit ?? {}) }
      if (info.rateLimitType === 'five_hour') next.fiveHour = win
      else if (typeof info.rateLimitType === 'string' && info.rateLimitType.startsWith('seven_day')) next.sevenDay = win
      s.rateLimit = next
    }
  }

  /**
   * Attach the run's Tauri event listeners. Idempotent — calling it again for
   * an already-listened run is a no-op, so it's safe to call on every view
   * (re)load. Listeners stay attached until the run emits `run:done`.
   */
  async function startListening(runId: string, projectId: string) {
    if (unlisteners.has(runId)) return
    const runsStore = useRunsStore()
    const remoteControl = useRemoteControlStore()
    const s = ensure(runId, projectId)
    const toolIndex = toolIndexes.get(runId)!
    const fns: UnlistenFn[] = []

    fns.push(
      await listen<{ run_id: string; line: string; is_stderr: boolean; level?: string }>(
        `run:output:${runId}`,
        (event) => {
          const { line, is_stderr, level } = event.payload
          s.outputLines.push({ text: line, isStderr: is_stderr })
          if (s.hasStreamEvents) {
            if (level && level !== 'error') s.entries.push({ kind: 'log', level, text: line })
            else if (is_stderr) s.entries.push({ kind: 'error', text: line })
          }
        },
      ),
    )

    // ── Batched stream-event processing ─────────────────────────────────────
    // Stream events arrive in rapid bursts (tool spam, or the flush released
    // when a permission is allowed). Applying + re-rendering per event freezes
    // the main thread. Buffer the raw payloads and drain them once per frame so
    // the UI re-renders at most ~once/frame no matter how fast events land. A
    // setTimeout fallback covers a hidden window (rAF is paused there) so a
    // background run's state still advances.
    let eventBuffer: unknown[] = []
    let flushScheduled = false
    function flushEvents() {
      flushScheduled = false
      if (!eventBuffer.length) return
      const batch = eventBuffer
      eventBuffer = []
      s.hasStreamEvents = true
      for (const payload of batch) {
        applyStreamEvent({ entries: s.entries, toolIndex }, payload)
        const p = payload as Record<string, unknown> | null
        // Capture slash commands advertised on system.init (Claude only).
        if (p && p.type === 'system' && p.subtype === 'init' && Array.isArray(p.slash_commands)) {
          s.slashCommands = (p.slash_commands as unknown[]).map((c) => String(c))
          const engine = runsStore.runs.find((r) => r.id === runId)?.engine
          cacheSlashCommands(engine, s.slashCommands)
        }
        // Capture real claude.ai rate-limit windows (Claude subscription only).
        captureRateLimit(s, p)
        // Track context-window occupancy from usage; reset on compaction.
        // Use the LATEST per-message `assistant` usage (the whole prior history
        // rides in as cache_read, so the newest message is the true current
        // window size). A high-water max would pin the meter to an earlier spike
        // and never fall after a compaction/resume shrinks the window — which is
        // exactly what Claude's own context indicator reports. The `result`
        // event's usage is a cumulative per-turn sum that over-counts, so it is
        // only a fallback for engines that never emit per-message usage.
        if (isCompactBoundary(payload)) {
          s.contextTokens = 0
          s.sawAssistantUsage = false
        } else {
          const ctx = extractContextTokens(payload)
          if (ctx !== null) {
            s.contextTokens = ctx
            s.sawAssistantUsage = true
          } else if (!s.sawAssistantUsage) {
            const total = extractTurnTotalTokens(payload)
            if (total !== null) s.contextTokens = total
          }
        }
      }
      // sessionId/model come from a system entry; scan once per flush, not per
      // event (the earlier per-event O(n) scan grew costly on long sessions).
      for (const e of s.entries) {
        if (e.kind === 'system' && e.sessionId) s.sessionId = e.sessionId
        if (e.kind === 'system' && e.model) s.model = e.model
      }
    }
    const scheduleFlush = () => {
      if (flushScheduled) return
      flushScheduled = true
      if (typeof document !== 'undefined' && document.hidden) setTimeout(flushEvents, 48)
      else requestAnimationFrame(flushEvents)
    }

    fns.push(
      await listen<unknown>(`run:event:${runId}`, (event) => {
        eventBuffer.push(event.payload)
        scheduleFlush()
      }),
    )

    fns.push(
      await listen<{ run_id: string; status: string }>(`run:done:${runId}`, (event) => {
        flushEvents() // drain any events still buffered this frame before stopping
        s.status = event.payload.status
        s.permissionQueue = []
        const r = runsStore.runs.find((x) => x.id === runId)
        if (r) r.status = event.payload.status as typeof r.status
        // Only refresh the shared run list when it actually belongs to this
        // run's project. With multi-project tabs the foreground RunView may be
        // showing a DIFFERENT project; refetching here would clobber its list
        // (`currentRun` would vanish, popping panels open). The owning project's
        // list refreshes on its own when its tab is next viewed (RunView mount),
        // and the in-place `r.status` above keeps it correct if it is loaded.
        if (runsStore.loadedProjectId === projectId) {
          runsStore.fetchRuns(projectId).catch(() => {})
        }
        stopListening(runId)
      }),
    )

    fns.push(
      await listen<{ run_id: string; source: string; percent: number }>(
        `run:budget_exceeded:${runId}`,
        (event) => {
          // A turn just tipped the global budget over. The backend will refuse
          // the next turn; flag it so the UI can warn and disable quick-send.
          s.budgetBlocked = true
          const pct = event.payload.percent
          const what = event.payload.source === 'plan' ? 'subscription plan' : 'token budget'
          const line = `⚠ Budget reached: ${pct}% of the ${what} limit. New turns are blocked (override per turn to continue).`
          s.outputLines.push({ text: line, isStderr: true })
          if (s.hasStreamEvents) s.entries.push({ kind: 'error', text: line })
        },
      ),
    )

    fns.push(
      await listen<PermissionRequest>(`run:permission_request:${runId}`, (event) => {
        const req = event.payload
        // When this run is actively driven by an AUTHENTICATED remote controller,
        // the human being asked is remote — the desktop must NOT auto-allow/deny
        // from its local per-project lists. Doing so races and beats the remote
        // user's Deny (a local auto-`allow` fired ~24ms before the relayed deny),
        // silently running a tool the remote user rejected. Defer to the remote:
        // just enqueue; the controller answers and `run:permission_resolved`
        // clears it. (A human at the desktop can still answer manually.)
        const rc = remoteControl.status
        const remoteDriven = !!rc?.session_authenticated && rc?.bound_run_id === req.run_id
        // AskUserQuestion must always reach the user — auto-deciding it would
        // submit empty answers. Other tools honor the project's standing
        // deny/allow choices (deny wins if both somehow apply).
        if (!remoteDriven && req.tool_name !== 'AskUserQuestion') {
          if (s.deniedTools.includes(req.tool_name)) {
            runsStore
              .respondPermission(req.run_id, req.request_id, 'deny', 'Auto-denied for this project')
              .catch(() => {})
            return
          }
          if (s.allowedTools.includes(req.tool_name)) {
            runsStore
              .respondPermission(req.run_id, req.request_id, 'allow', 'Auto-allowed for this session')
              .catch(() => {})
            return
          }
        }
        s.permissionQueue.push(req)
      }),
    )

    fns.push(
      // A remote Controller (or the broker) answered a permission request. The
      // desktop's own reply path already shifts the queue, so this listener
      // mainly covers the remote case: drop the still-pending prompt by
      // request_id. Idempotent — a no-op if it was already removed.
      await listen<{ request_id: string }>(`run:permission_resolved:${runId}`, (event) => {
        const { request_id } = event.payload
        const i = s.permissionQueue.findIndex((p) => p.request_id === request_id)
        if (i !== -1) s.permissionQueue.splice(i, 1)
      }),
    )

    unlisteners.set(runId, fns)
  }

  function stopListening(runId: string) {
    const fns = unlisteners.get(runId)
    if (fns) {
      fns.forEach((f) => f())
      unlisteners.delete(runId)
    }
  }

  function rememberAllowedTool(runId: string, tool: string) {
    const s = sessions.get(runId)
    if (!s) return
    if (!s.allowedTools.includes(tool)) s.allowedTools.push(tool)
    const d = s.deniedTools.indexOf(tool)
    if (d !== -1) s.deniedTools.splice(d, 1)
    // Persist per project so "allow always" survives new runs and app restarts.
    useToolPermissionsStore().allow(s.projectId, tool)
  }

  function rememberDeniedTool(runId: string, tool: string) {
    const s = sessions.get(runId)
    if (!s) return
    if (!s.deniedTools.includes(tool)) s.deniedTools.push(tool)
    const a = s.allowedTools.indexOf(tool)
    if (a !== -1) s.allowedTools.splice(a, 1)
    // Persist per project so "deny always" survives new runs and app restarts.
    useToolPermissionsStore().deny(s.projectId, tool)
  }

  /**
   * Re-seed the live allow/deny lists of every session in a project from the
   * persisted store. Called after the user edits standing permissions so the
   * change takes effect on runs that are already open (not just future ones).
   */
  function syncToolPermissions(projectId: string) {
    const perms = useToolPermissionsStore()
    sessions.forEach((s) => {
      if (s.projectId === projectId) {
        s.allowedTools = perms.getAllow(projectId)
        s.deniedTools = perms.getDeny(projectId)
      }
    })
  }

  function shiftPermission(runId: string) {
    sessions.get(runId)?.permissionQueue.shift()
  }

  /** Drop a run's session entirely (stop listeners + free memory). */
  function discard(runId: string) {
    stopListening(runId)
    sessions.delete(runId)
    toolIndexes.delete(runId)
  }

  /** Run ids that are currently streaming — for live status indicators. */
  const runningIds = computed(() => {
    const ids: string[] = []
    sessions.forEach((s, id) => {
      if (s.status === 'running') ids.push(id)
    })
    return ids
  })

  return {
    sessions,
    get,
    ensure,
    reset,
    pushUser,
    setStatus,
    isListening,
    startListening,
    stopListening,
    rememberAllowedTool,
    rememberDeniedTool,
    syncToolPermissions,
    shiftPermission,
    discard,
    runningIds,
    cachedSlashCommands,
  }
})
