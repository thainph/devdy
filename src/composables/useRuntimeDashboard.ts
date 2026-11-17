// Runtime Dashboard data: the live runs, their per-run activity, resources,
// today's finished runs, sessions outside Devdy and the 5-hour quotas.
//
// Sources: `get_runtime_snapshot` (polled ~3 s while the screen is visible) +
// `list_active_runs` + the liveRuns store. While the backend does not provide
// the snapshot (the command errors), the same picture is built from
// `list_active_runs`, liveRuns (entries, contextTokens, attention) and the
// background-processes snapshot; what has no source stays empty.
//
// Performance: the cards never read the stream directly. `sessions` is a
// summary rebuilt at most once a second (and right after a poll or an action);
// only the selected session streams its log (`useSessionLog`).
import { computed, onBeforeUnmount, onMounted, ref, shallowRef, toRaw, watch, type Ref } from 'vue'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { useI18n } from 'vue-i18n'
import { invoke } from '@/lib/tauri'
import { useLiveRunsStore, type LiveSession } from '@/stores/liveRuns'
import { useBackgroundProcessesStore } from '@/stores/backgroundProcesses'
import { useAppSettingsStore } from '@/stores/appSettings'
import { useRunsStore } from '@/stores/runs'
import { mergeContextModel, resolveContextLimit } from '@/lib/contextLimits'
import { entryKey, parseStreamLogWindow, type StreamEntry } from '@/lib/streamEvents'
import {
  CMD_RUNTIME_SNAPSHOT,
  type ExternalSession,
  type RuntimeResources,
  type RuntimeRun,
  type RuntimeSnapshot,
  type RuntimeToday,
} from '@/lib/runtimeSnapshot'
import {
  activityBuckets,
  classifyState,
  contextPercent,
  lastEvent,
  logLine,
  noteActivity,
  parseTime,
  pushQuotaSample,
  pushSample,
  quotaForecast,
  runTitle,
  toolSummary,
  type ActiveRun,
  type ActivityTrack,
  type LogLine,
  type NowLine,
  type QuotaForecast,
  type QuotaSample,
  type RtSession,
} from '@/lib/runtimeDashboard'

const POLL_MS = 3000
const TICK_MS = 1000
const QUOTA_POLL_MS = 60_000
const EVENT_DEBOUNCE_MS = 300
const CPU_HISTORY = 12

// Kept across visits (module state): the fallback activity tracks, CPU history
// and quota readings only grow while the screen is shown, so a remount should
// not start them over.
const tracks = new Map<string, ActivityTrack>()
const firstSeen = new WeakMap<object, number>()
let cpuHistoryFallback: number[] = []
const quotaSamples: Record<'claude' | 'codex', QuotaSample[]> = { claude: [], codex: [] }

function seenAt(o: object, now: number): number {
  const t = firstSeen.get(o)
  if (t !== undefined) return t
  firstSeen.set(o, now)
  return now
}

interface PlanWindowData {
  utilization: number | null
  resets_at: string | null
}
interface PlanUsageData {
  captured_at: string
  rate_limits_available: boolean
  windows: { five_hour: PlanWindowData }
}

export interface QuotaView {
  engine: 'claude' | 'codex'
  /** 0-100, null = no data yet. */
  pct: number | null
  resetsAt: number | null
  forecast: QuotaForecast
}

export interface ResourcesView extends RuntimeResources {
  /** CPU as a share of the whole machine (0-100). */
  cpuShare: number
  /** True when built from the background-processes snapshot. */
  fallback: boolean
}

/** The tool entry still running in the current turn (no result yet). */
function runningToolEntry(entries: readonly StreamEntry[]): Extract<StreamEntry, { kind: 'tool' }> | null {
  for (let i = entries.length - 1; i >= 0; i--) {
    const e = entries[i]
    if (e.kind === 'result' || e.kind === 'user') return null
    if (e.kind === 'tool') return e.result === undefined ? e : null
  }
  return null
}

function questionText(input: unknown): string {
  return toolSummary('AskUserQuestion', input)
}

export function useRuntimeDashboard() {
  const { t } = useI18n()
  const live = useLiveRunsStore()
  const bgProcs = useBackgroundProcessesStore()
  const appSettings = useAppSettingsStore()

  const activeRuns = shallowRef<ActiveRun[]>([])
  const snapshot = shallowRef<RuntimeSnapshot | null>(null)
  /** False while `get_runtime_snapshot` is unavailable (fallback mode). */
  const hasSnapshot = ref(false)
  const loaded = ref(false)
  const loadError = ref<string | null>(null)
  const sessions = shallowRef<RtSession[]>([])
  const now = ref(Date.now())
  const planClaude = shallowRef<PlanUsageData | null>(null)
  const planCodex = shallowRef<PlanUsageData | null>(null)

  const contextOverride = computed(() => {
    const v = Number(appSettings.settings?.context_limit_override)
    return Number.isFinite(v) && v > 0 ? v : null
  })

  // ── Build the session summaries ─────────────────────────────────────────
  function bgByRun(): Map<string, { cpu: number; rss: number }> {
    const out = new Map<string, { cpu: number; rss: number }>()
    for (const e of bgProcs.snapshot?.entries ?? []) {
      if (e.kind !== 'run' || !e.run_id) continue
      const cur = out.get(e.run_id) ?? { cpu: 0, rss: 0 }
      out.set(e.run_id, { cpu: cur.cpu + e.cpu, rss: cur.rss + e.rss_kb })
    }
    return out
  }

  function nowLine(
    ar: ActiveRun,
    s: LiveSession | undefined,
    r: RuntimeRun | undefined,
    waiting: boolean,
    t0: number,
  ): NowLine {
    const head = s?.permissionQueue[0]
    if (waiting) {
      const since = parseTime(ar.waiting_since) ?? (head ? seenAt(head, t0) : null)
      if (head?.tool_name === 'AskUserQuestion' || (!head && r?.awaitingQuestion)) {
        const detail = head ? questionText(head.tool_input) : r?.awaitingQuestion?.question ?? ''
        return { kind: 'question', tool: null, detail, since: parseTime(r?.awaitingQuestion?.since) ?? since }
      }
      return {
        kind: 'permission',
        tool: head?.tool_name ?? null,
        detail: head ? toolSummary(head.tool_name, head.tool_input) : '',
        since,
      }
    }
    if (r?.currentTool) {
      return { kind: 'tool', tool: r.currentTool.name, detail: r.currentTool.summary, since: parseTime(r.currentTool.startedAt) }
    }
    const e = s ? runningToolEntry(s.entries) : null
    if (e) return { kind: 'tool', tool: e.name, detail: toolSummary(e.name, e.input), since: seenAt(e, t0) }
    // Nothing known to be running (Codex reports tools once finished): the
    // latest event instead of an empty line.
    const last = s ? lastEvent(s.entries) : null
    if (last) return { kind: 'last', tool: last.tool, detail: last.text, since: null }
    return { kind: 'none', tool: null, detail: '', since: null }
  }

  function buildRun(ar: ActiveRun, snap: Map<string, RuntimeRun>, bg: Map<string, { cpu: number; rss: number }>, t0: number): RtSession {
    const s = live.sessions.get(ar.run_id)
    const r = snap.get(ar.run_id)
    const listening = !!s && live.isListening(ar.run_id)
    const pending = listening ? s.permissionQueue.length : ar.pending_permissions
    const track = s ? noteActivity(tracks, ar.run_id, s.entries.length, t0) : tracks.get(ar.run_id)
    const lastEventAt = r ? parseTime(r.lastEventAt) : track?.lastEventAt ?? null
    const conductor = ar.role === 'conductor'
    const line = nowLine(ar, s, r, pending > 0, t0)
    const state = classifyState({
      pending,
      conductor,
      currentTool: line.kind === 'tool' ? line.tool : null,
      lastEventAt,
      attention: s?.attention.map((a) => a.kind) ?? [],
      now: t0,
    })
    if (state === 'idle') line.kind = 'wait'
    // The snapshot's 0 means "just compacted"; the live store's 0 means "nothing seen yet".
    const tokens = r?.contextTokens ?? (s?.contextTokens || null)
    // `contextWindow` is not filled by the backend: the model decides.
    const limit = r?.contextWindow ?? resolveContextLimit(mergeContextModel(s?.model, ar.model), contextOverride.value)
    const res = r ? { cpu: r.cpuPercent, rss: r.rssKb } : bg.get(ar.run_id)
    return {
      id: ar.run_id,
      runId: ar.run_id,
      projectId: ar.project_id,
      projectName: ar.project_name,
      title: runTitle(ar) ?? t('dashboard.untitled'),
      engine: ar.engine === 'codex' ? 'codex' : 'claude',
      role: conductor ? 'conductor' : ar.conductor_run_id ? 'worker' : 'session',
      conductorId: ar.conductor_run_id,
      state,
      waitingSince: state === 'waiting' ? line.since : null,
      startedAt: parseTime(ar.started_at ?? ar.created_at),
      now: line,
      ctxPercent: contextPercent(tokens, limit),
      spark: r?.eventsPerMinute ?? (track ? activityBuckets(track, t0) : []),
      cpuPercent: res?.cpu ?? null,
      rssKb: res?.rss ?? null,
    }
  }

  function buildExternal(e: ExternalSession): RtSession {
    const projectName = e.projectName ?? (e.cwd ? e.cwd.split('/').filter(Boolean).pop() ?? '' : '')
    return {
      id: `ext:${e.id}`,
      runId: null,
      projectId: e.projectId,
      projectName,
      title: e.title,
      engine: e.engine === 'codex' ? 'codex' : 'claude',
      role: 'external',
      conductorId: null,
      state: 'external',
      waitingSince: null,
      startedAt: null,
      now: { kind: 'none', tool: null, detail: '', since: parseTime(e.lastActivityAt) },
      ctxPercent: null,
      spark: [],
      cpuPercent: null,
      rssKb: null,
    }
  }

  function rebuild() {
    const t0 = Date.now()
    now.value = t0
    const snap = new Map((snapshot.value?.runs ?? []).map((r) => [r.runId, r]))
    const bg = bgByRun()
    const out = activeRuns.value.map((ar) => buildRun(ar, snap, bg, t0))
    for (const e of snapshot.value?.external ?? []) out.push(buildExternal(e))
    for (const id of tracks.keys()) if (!activeRuns.value.some((r) => r.run_id === id)) tracks.delete(id)
    sessions.value = out
  }

  // ── External sessions (detail panel) ────────────────────────────────────
  const externalById = computed(() => new Map((snapshot.value?.external ?? []).map((e) => [`ext:${e.id}`, e])))

  // ── Resources / today ───────────────────────────────────────────────────
  const cores = typeof navigator !== 'undefined' && navigator.hardwareConcurrency > 0 ? navigator.hardwareConcurrency : 1
  const resources = computed<ResourcesView | null>(() => {
    const r = snapshot.value?.resources
    if (r) return { ...r, cpuShare: Math.min(100, r.cpuPercent / cores), fallback: false }
    const bg = bgProcs.snapshot
    if (!bg) return null
    const cpu = bg.entries.reduce((n, e) => n + e.cpu, 0)
    return {
      cpuPercent: cpu,
      rssKb: bg.entries.reduce((n, e) => n + e.rss_kb, 0),
      totalMemKb: 0,
      processCount: bg.counts.total_processes,
      orphanCount: bg.counts.orphan + bg.counts.leftover,
      staleWorktreeCount: null,
      cpuHistory: cpuHistoryFallback,
      cpuShare: Math.min(100, cpu / cores),
      fallback: true,
    }
  })
  const today = computed<RuntimeToday | null>(() => snapshot.value?.today ?? null)

  // ── Quotas ──────────────────────────────────────────────────────────────
  async function loadQuotas() {
    const [c, x] = await Promise.allSettled([
      invoke<PlanUsageData | null>('get_plan_usage'),
      invoke<PlanUsageData | null>('get_plan_usage_codex'),
    ])
    if (c.status === 'fulfilled') planClaude.value = c.value
    if (x.status === 'fulfilled') planCodex.value = x.value
    const at = Date.now()
    for (const [engine, plan] of [['claude', planClaude.value], ['codex', planCodex.value]] as const) {
      const pct = plan?.windows?.five_hour?.utilization
      if (typeof pct === 'number') quotaSamples[engine] = pushQuotaSample(quotaSamples[engine], { at, pct })
    }
  }

  function quotaOf(engine: 'claude' | 'codex', plan: PlanUsageData | null): QuotaView {
    const w = plan?.rate_limits_available === false ? null : plan?.windows?.five_hour
    const pct = typeof w?.utilization === 'number' ? w.utilization : null
    let resetsAt = parseTime(w?.resets_at)
    // A window whose reset passed is a new one: its old reading means nothing.
    const stale = resetsAt !== null && resetsAt <= now.value
    if (stale) resetsAt = null
    const shown = stale ? null : pct
    return { engine, pct: shown, resetsAt, forecast: quotaForecast(shown, resetsAt, quotaSamples[engine], now.value) }
  }
  const quotas = computed<QuotaView[]>(() => [quotaOf('claude', planClaude.value), quotaOf('codex', planCodex.value)])

  // ── Polling ─────────────────────────────────────────────────────────────
  let inflight: Promise<void> | null = null

  /** Attach liveRuns listeners to live runs this window does not follow yet (e.g. after a reload). */
  function follow(list: ActiveRun[]) {
    for (const r of list) {
      if (!live.isListening(r.run_id)) live.startListening(r.run_id, r.project_id).catch(() => {})
    }
  }

  async function poll(): Promise<void> {
    if (inflight) return inflight
    inflight = (async () => {
      const [runs, snap] = await Promise.allSettled([
        invoke<ActiveRun[]>('list_active_runs'),
        invoke<RuntimeSnapshot>(CMD_RUNTIME_SNAPSHOT),
      ])
      if (runs.status === 'fulfilled') {
        activeRuns.value = runs.value
        loadError.value = null
        follow(runs.value)
      } else {
        loadError.value = String(runs.reason)
      }
      if (snap.status === 'fulfilled' && snap.value) {
        snapshot.value = snap.value
        if (!hasSnapshot.value) {
          hasSnapshot.value = true
          stopBgWatch()
        }
      } else {
        snapshot.value = null
        hasSnapshot.value = false
        if (pollTimer) startBgWatch()
      }
      loaded.value = true
      rebuild()
    })().finally(() => {
      inflight = null
    })
    return inflight
  }

  // Fallback resources: the background-processes snapshot, refreshed ~5 s
  // while visible (the store's own poller).
  let endBgWatch: (() => void) | null = null
  function startBgWatch() {
    if (!endBgWatch) endBgWatch = bgProcs.watch(5000)
  }
  function stopBgWatch() {
    endBgWatch?.()
    endBgWatch = null
  }
  watch(
    () => bgProcs.snapshot,
    (snap) => {
      if (!snap || hasSnapshot.value) return
      cpuHistoryFallback = pushSample(cpuHistoryFallback, snap.entries.reduce((n, e) => n + e.cpu, 0), CPU_HISTORY)
    },
  )

  let pollTimer: ReturnType<typeof setInterval> | null = null
  let tickTimer: ReturnType<typeof setInterval> | null = null
  let quotaTimer: ReturnType<typeof setInterval> | null = null
  let debounceTimer: ReturnType<typeof setTimeout> | null = null
  let unlisten: UnlistenFn[] = []
  let disposed = false

  function visible(): boolean {
    return typeof document === 'undefined' || !document.hidden
  }

  function start() {
    if (pollTimer || disposed) return
    void poll()
    void loadQuotas()
    pollTimer = setInterval(() => void poll(), POLL_MS)
    tickTimer = setInterval(rebuild, TICK_MS)
    quotaTimer = setInterval(() => void loadQuotas(), QUOTA_POLL_MS)
    if (!hasSnapshot.value && loaded.value) startBgWatch()
  }
  function stop() {
    for (const tm of [pollTimer, tickTimer, quotaTimer]) if (tm) clearInterval(tm)
    pollTimer = tickTimer = quotaTimer = null
    stopBgWatch()
  }
  function onVisibility() {
    if (visible()) start()
    else stop()
  }

  /** Re-poll soon (coalesced) — a run started / finished, a prompt changed. */
  function soon() {
    if (disposed || !pollTimer) return
    if (debounceTimer) clearTimeout(debounceTimer)
    debounceTimer = setTimeout(() => {
      debounceTimer = null
      void poll()
    }, EVENT_DEBOUNCE_MS)
  }

  // A prompt opening / closing or a run settling: refresh at once instead of
  // waiting for the next poll.
  const liveSignature = computed(() => {
    const parts: string[] = []
    live.sessions.forEach((s, id) => parts.push(`${id}:${s.status}:${s.permissionQueue.length}`))
    return parts.sort().join('|')
  })
  watch(liveSignature, () => {
    rebuild()
    soon()
  })

  async function on(event: string, handler: () => void) {
    const off = await listen(event, handler)
    if (disposed) off()
    else unlisten.push(off)
  }

  onMounted(async () => {
    document.addEventListener('visibilitychange', onVisibility)
    if (visible()) start()
    try {
      await Promise.all([
        on('runs:list_changed', soon),
        on('plan_usage_updated', () => void loadQuotas()),
      ])
    } catch {
      /* outside the Tauri shell */
    }
  })
  onBeforeUnmount(() => {
    disposed = true
    document.removeEventListener('visibilitychange', onVisibility)
    stop()
    if (debounceTimer) clearTimeout(debounceTimer)
    for (const u of unlisten) u()
    unlisten = []
  })

  return {
    sessions,
    activeRuns,
    now,
    loaded,
    loadError,
    hasSnapshot,
    resources,
    today,
    quotas,
    externalById,
    refresh: poll,
    rebuild,
  }
}

/** A line of the detail log; `at` is null when the time is unknown (older entries). */
export interface TimedLogLine extends LogLine {
  key: string
  at: number | null
}

const HISTORY_RECORDS = 80
const LIVE_LINES = 150

/**
 * The selected session's log: its live entries (streaming), preceded — when the
 * live store holds little of it (a run followed only since a reload, a run
 * first seen on a resume, an external session) — by the tail of its on-disk
 * log, loaded once per selection. Only this session is read, so the other
 * runs' streams never touch the dashboard.
 */
export function useSessionLog(runId: Ref<string | null>) {
  const live = useLiveRunsStore()
  const runsStore = useRunsStore()
  const history = shallowRef<StreamEntry[]>([])
  const loading = ref(false)
  // An entry has no timestamp of its own: lines get the time they first
  // showed up here; the ones already there on selection stay without one.
  const stamps = new WeakMap<object, number>()
  let baseline = 0
  let token = 0

  async function loadHistory(id: string, before?: number) {
    const mine = ++token
    loading.value = true
    try {
      const page = await runsStore.getRunLogPage(id, before, HISTORY_RECORDS)
      if (mine === token) history.value = parseStreamLogWindow(page.records).entries
    } catch {
      if (mine === token) history.value = []
    } finally {
      if (mine === token) loading.value = false
    }
  }

  function select(id: string | null) {
    history.value = []
    token++
    loading.value = false
    baseline = 0
    if (!id) return
    const s = live.sessions.get(id)
    baseline = s?.entries.length ?? 0
    if (!s) {
      void loadHistory(id) // not followed live (an external session's mirror)
    } else if (s.historyEnd !== null) {
      void loadHistory(id, s.historyEnd) // earlier turns, before what streams live
    } else if (s.entries.length === 0) {
      void loadHistory(id)
    }
  }
  watch(runId, select, { immediate: true })

  // A mirrored transcript changed: reload the tail of a session not streamed live.
  let unlisten: UnlistenFn | null = null
  let disposed = false
  onMounted(async () => {
    try {
      const off = await listen<{ run_id?: string }>('sessions:changed', (e) => {
        const id = runId.value
        if (id && e.payload?.run_id === id && !live.sessions.has(id)) void loadHistory(id)
      })
      if (disposed) off()
      else unlisten = off
    } catch {
      /* outside the Tauri shell */
    }
  })
  onBeforeUnmount(() => {
    disposed = true
    unlisten?.()
  })

  const lines = computed<TimedLogLine[]>(() => {
    const id = runId.value
    const out: TimedLogLine[] = []
    if (!id) return out
    for (const e of history.value) {
      const l = logLine(e)
      if (l) out.push({ ...l, key: `h${entryKey(e)}`, at: null })
    }
    const entries = live.sessions.get(id)?.entries ?? []
    const t0 = Date.now()
    for (let i = Math.max(0, entries.length - LIVE_LINES); i < entries.length; i++) {
      const raw = toRaw(entries[i])
      const l = logLine(raw)
      if (!l) continue
      let at = stamps.get(raw) ?? null
      if (at === null && i >= baseline) {
        at = t0
        stamps.set(raw, t0)
      }
      out.push({ ...l, key: `l${entryKey(raw)}`, at })
    }
    return out
  })

  return { lines, loading }
}
