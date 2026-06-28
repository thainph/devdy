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
  /** Tool names the user chose to auto-allow for the rest of this run. */
  allowedTools: string[]
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
        model: null as string | null,
        contextTokens: 0,
        sawAssistantUsage: false,
        slashCommands: [] as string[],
        rateLimit: null as RateLimitWindows | null,
        budgetBlocked: false,
      }) as LiveSession
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

    fns.push(
      await listen<unknown>(`run:event:${runId}`, (event) => {
        s.hasStreamEvents = true
        applyStreamEvent({ entries: s.entries, toolIndex }, event.payload)
        for (const e of s.entries) {
          if (e.kind === 'system' && e.sessionId) s.sessionId = e.sessionId
          if (e.kind === 'system' && e.model) s.model = e.model
        }
        // Capture slash commands advertised on system.init (Claude only).
        const p = event.payload as Record<string, unknown> | null
        if (p && p.type === 'system' && p.subtype === 'init' && Array.isArray(p.slash_commands)) {
          s.slashCommands = (p.slash_commands as unknown[]).map((c) => String(c))
          const engine = runsStore.runs.find((r) => r.id === runId)?.engine
          cacheSlashCommands(engine, s.slashCommands)
        }
        // Capture real claude.ai rate-limit windows (Claude subscription only).
        captureRateLimit(s, p)
        // Track context-window occupancy from usage; reset on compaction.
        // Prefer per-message `assistant` usage (true window size, grows turn
        // over turn) and keep the high-water mark — a turn's calls only add to
        // the window, so the largest single call is its occupancy. The `result`
        // event's usage is a cumulative per-turn sum that over-counts, so it is
        // only a fallback for engines that never emit per-message usage.
        if (isCompactBoundary(event.payload)) {
          s.contextTokens = 0
          s.sawAssistantUsage = false
        } else {
          const ctx = extractContextTokens(event.payload)
          if (ctx !== null) {
            s.contextTokens = Math.max(s.contextTokens, ctx)
            s.sawAssistantUsage = true
          } else if (!s.sawAssistantUsage) {
            const total = extractTurnTotalTokens(event.payload)
            if (total !== null) s.contextTokens = total
          }
        }
      }),
    )

    fns.push(
      await listen<{ run_id: string; status: string }>(`run:done:${runId}`, (event) => {
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
        // AskUserQuestion must always reach the user — auto-allowing it would
        // submit empty answers. Other tools honor the per-session allowlist.
        if (req.tool_name !== 'AskUserQuestion' && s.allowedTools.includes(req.tool_name)) {
          runsStore
            .respondPermission(req.run_id, req.request_id, 'allow', 'Auto-allowed for this session')
            .catch(() => {})
          return
        }
        s.permissionQueue.push(req)
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
    if (s && !s.allowedTools.includes(tool)) s.allowedTools.push(tool)
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
    shiftPermission,
    discard,
    runningIds,
    cachedSlashCommands,
  }
})
