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
// summary rebuilt at most once a second (and right after a poll or an action).
import { computed, onBeforeUnmount, onMounted, ref, shallowRef, watch } from 'vue'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { useI18n } from 'vue-i18n'
import { invoke } from '@/lib/tauri'
import { useLiveRunsStore, type LiveSession } from '@/stores/liveRuns'
import { useBackgroundProcessesStore } from '@/stores/backgroundProcesses'
import { useAppSettingsStore } from '@/stores/appSettings'
import { mergeContextModel, resolveContextLimit } from '@/lib/contextLimits'
import type { StreamEntry } from '@/lib/streamEvents'
import { sumBuckets } from '@/lib/runtimeCharts'
import {
  ACTIVITY_BUCKETS,
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
  isMissingCommandError,
  lastEvent,
  noteActivity,
  parseTime,
  pushQuotaSample,
  pushSample,
  quotaForecast,
  runTitle,
  toolSummary,
  type ActiveRun,
  type ActivityTrack,
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
const RAM_HISTORY = 40

// Kept across visits (module state): the fallback activity tracks, CPU / RAM
// history and quota readings only grow while the screen is shown, so a remount
// should not start them over.
const tracks = new Map<string, ActivityTrack>()
const firstSeen = new WeakMap<object, number>()
let cpuHistoryFallback: number[] = []
/** RAM of Devdy's processes (KB), one sample per poll — the snapshot keeps no RAM history. */
let ramHistory: number[] = []
/** Readings per quota key: `codex`, `claude` (default account) or `claude:<account id>`. */
const quotaSamples: Record<string, QuotaSample[]> = {}

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

/** The fields of `list_claude_accounts` the quota card needs. */
interface ClaudeAccountLite {
  id: string
  label: string
  email: string | null
  is_default: boolean
}

export interface QuotaView {
  /** `codex`, `claude`, or `claude:<account id>` when several Claude accounts are set up. */
  key: string
  engine: 'claude' | 'codex'
  /** The Claude account's name when several are set up, else null. */
  label: string | null
  /** That account is the default one. */
  isDefault: boolean
  /** 0-100, null = no data yet. */
  pct: number | null
  resetsAt: number | null
  forecast: QuotaForecast
}

export interface ResourcesView extends RuntimeResources {
  /** CPU as a share of the whole machine (0-100). */
  cpuShare: number
  /** `cpuHistory` as shares of the whole machine (0-100), oldest first. */
  cpuShareHistory: number[]
  /** RAM samples (KB) kept by this window, oldest first. */
  ramHistory: number[]
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
  /** The last poll of the snapshot failed; the one shown is older. */
  const snapshotStale = ref(false)
  const loaded = ref(false)
  const loadError = ref<string | null>(null)
  const sessions = shallowRef<RtSession[]>([])
  const now = ref(Date.now())
  const planClaude = shallowRef<PlanUsageData | null>(null)
  const planCodex = shallowRef<PlanUsageData | null>(null)
  /** One entry per Claude account when there are several (each has its own 5-hour window). */
  const planAccounts = shallowRef<{ id: string; label: string; isDefault: boolean; plan: PlanUsageData | null }[]>([])

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
    // latest tool call instead of an empty line.
    if (r?.lastTool) return { kind: 'last', tool: r.lastTool.name, detail: r.lastTool.summary, since: null }
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
      ended: ar.live === false,
      liveWorkers: conductor ? activeRuns.value.filter((x) => x.conductor_run_id === ar.run_id).length : 0,
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
      live: ar.live !== false,
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
      live: false,
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
    const share = (v: number) => Math.min(100, v / cores)
    if (r) return { ...r, cpuShare: share(r.cpuPercent), cpuShareHistory: r.cpuHistory.map(share), ramHistory, fallback: false }
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
      cpuShare: share(cpu),
      cpuShareHistory: cpuHistoryFallback.map(share),
      ramHistory,
      fallback: true,
    }
  })
  const today = computed<RuntimeToday | null>(() => snapshot.value?.today ?? null)
  /** Events per minute of every session added up, the last 20 minutes (oldest first). */
  const activity = computed(() => sumBuckets(sessions.value.map((s) => s.spark), ACTIVITY_BUCKETS))

  // ── Quotas ──────────────────────────────────────────────────────────────
  async function loadQuotas() {
    const [c, x, a] = await Promise.allSettled([
      invoke<PlanUsageData | null>('get_plan_usage'),
      invoke<PlanUsageData | null>('get_plan_usage_codex'),
      invoke<ClaudeAccountLite[]>('list_claude_accounts'),
    ])
    if (c.status === 'fulfilled') planClaude.value = c.value
    if (x.status === 'fulfilled') planCodex.value = x.value
    const accounts = a.status === 'fulfilled' ? a.value : []
    if (accounts.length > 1) {
      const plans = await Promise.allSettled(accounts.map((acc) => invoke<PlanUsageData | null>('get_plan_usage', { accountId: acc.id })))
      planAccounts.value = accounts.map((acc, i) => {
        const r = plans[i]
        const prev = planAccounts.value.find((p) => p.id === acc.id)?.plan ?? null
        return {
          id: acc.id,
          label: acc.label || acc.email || acc.id,
          isDefault: acc.is_default,
          plan: r?.status === 'fulfilled' ? r.value : prev,
        }
      })
    } else if (a.status === 'fulfilled') {
      planAccounts.value = []
    }
    const at = Date.now()
    const readings: [string, PlanUsageData | null][] = [
      ['claude', planClaude.value],
      ['codex', planCodex.value],
      ...planAccounts.value.map((p) => [`claude:${p.id}`, p.plan] as [string, PlanUsageData | null]),
    ]
    for (const [key, plan] of readings) {
      const pct = plan?.windows?.five_hour?.utilization
      if (typeof pct === 'number') quotaSamples[key] = pushQuotaSample(quotaSamples[key] ?? [], { at, pct })
    }
  }

  function quotaOf(
    engine: 'claude' | 'codex',
    plan: PlanUsageData | null,
    key: string = engine,
    label: string | null = null,
    isDefault = false,
  ): QuotaView {
    const w = plan?.rate_limits_available === false ? null : plan?.windows?.five_hour
    const pct = typeof w?.utilization === 'number' ? w.utilization : null
    let resetsAt = parseTime(w?.resets_at)
    // A window whose reset passed is a new one: its old reading means nothing.
    const stale = resetsAt !== null && resetsAt <= now.value
    if (stale) resetsAt = null
    const shown = stale ? null : pct
    return { key, engine, label, isDefault, pct: shown, resetsAt, forecast: quotaForecast(shown, resetsAt, quotaSamples[key] ?? [], now.value) }
  }
  const quotas = computed<QuotaView[]>(() => {
    const claude = planAccounts.value.length > 1
      ? planAccounts.value.map((p) => quotaOf('claude', p.plan, `claude:${p.id}`, p.label, p.isDefault))
      : [quotaOf('claude', planClaude.value)]
    return [...claude, quotaOf('codex', planCodex.value)]
  })

  // ── Polling ─────────────────────────────────────────────────────────────
  let inflight: Promise<void> | null = null

  /**
   * Fallback only (no runtime snapshot from the backend): attach liveRuns
   * listeners to live runs this window does not follow yet, so their streams
   * feed the "now" line. With the snapshot the dashboard reads no stream at all
   * — following every run would make this window parse every session's output
   * for as long as they run, dashboard open or not. A conductor between turns
   * is skipped.
   */
  function follow(list: ActiveRun[]) {
    if (hasSnapshot.value) return
    for (const r of list) {
      if (r.live !== false && !live.isListening(r.run_id)) live.startListening(r.run_id, r.project_id).catch(() => {})
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
      } else {
        loadError.value = String(runs.reason)
      }
      if (snap.status === 'fulfilled' && snap.value) {
        ramHistory = pushSample(ramHistory, snap.value.resources.rssKb, RAM_HISTORY)
        snapshot.value = snap.value
        snapshotStale.value = false
        if (!hasSnapshot.value) {
          hasSnapshot.value = true
          stopBgWatch()
        }
      } else if (snapshot.value && !(snap.status === 'rejected' && isMissingCommandError(snap.reason))) {
        // A transient failure: keep the last good snapshot, flagged as stale.
        snapshotStale.value = true
      } else {
        // No backend support (or never got one): build what we can ourselves.
        snapshot.value = null
        snapshotStale.value = false
        hasSnapshot.value = false
        if (pollTimer) startBgWatch()
      }
      // After the snapshot settled: only the fallback needs the streams.
      if (runs.status === 'fulfilled') follow(runs.value)
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
      ramHistory = pushSample(ramHistory, snap.entries.reduce((n, e) => n + e.rss_kb, 0), RAM_HISTORY)
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
    snapshotStale,
    resources,
    today,
    activity,
    quotas,
    externalById,
    refresh: poll,
    rebuild,
  }
}
