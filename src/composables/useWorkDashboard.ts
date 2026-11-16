import { computed, onBeforeUnmount, onMounted, ref, watch, type Ref } from 'vue'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { invoke } from '@/lib/tauri'
import { useFocusStore } from '@/stores/focus'
import { usePrInboxStore, type PrInboxItem } from '@/stores/prInbox'
import { useDeliveryRadarStore } from '@/stores/deliveryRadar'
import { useProjectsStore, type Project } from '@/stores/projects'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { SLACK_THREADS_CHANGED, type SlackThreadSummary } from '@/stores/slackThreads'
import { SLACK_CUSTOMERS_CHANGED } from '@/stores/slackCustomers'
import { makeBotPredicate } from '@/lib/prInboxFilters'
import { buildSlackRows, type SlackThreadActivity } from '@/lib/dashboardSlack'
import { compareHealth, projectHealth, type HealthBoard, type ProjectHealth } from '@/lib/projectHealth'
import { rankPrs } from '@/lib/prPriority'
import { projectRows, projectSignals } from '@/lib/dashboardLayout'
import {
  dashboardRuns, dashboardTodos, inProject, type ActiveRun, type AttentionKey,
} from '@/lib/workDashboard'

/** One widget's data source state, for its header (updated / error / loading). */
export interface SourceState {
  loading: boolean
  error: string | null
  /** Epoch ms the shown data was produced, if known. */
  updatedAt: number | null
}

const EVENT_DEBOUNCE_MS = 300

function parseTime(iso: string | null | undefined): number | null {
  if (!iso) return null
  const t = Date.parse(iso)
  return Number.isNaN(t) ? null : t
}

/** The project only uses GitLab: the radar (GitHub Projects) cannot judge it. */
function gitlabOnly(p: Project): boolean {
  return (
    !p.github_project_board_url &&
    p.gitlab_account_ids.length > 0 &&
    p.github_account_ids.length === 0 &&
    !p.github_account_id
  )
}

/**
 * Everything the Work Dashboard shows, filtered by `projectFilter` (null = all
 * projects) and measured against `now`. Reuses the existing stores (Focus, PR
 * Inbox, Delivery Radar, live runs) and loads the Slack threads + their
 * activity and the live sessions itself. No new poller: data refreshes on the
 * existing events, when the screen opens and on `refresh()`.
 */
export function useWorkDashboard(projectFilter: Ref<string | null>, now: Ref<number>) {
  const focus = useFocusStore()
  const prInbox = usePrInboxStore()
  const radar = useDeliveryRadarStore()
  const projects = useProjectsStore()
  const live = useLiveRunsStore()

  // ── Slack ────────────────────────────────────────────────────────────────
  const slackThreads = ref<SlackThreadSummary[]>([])
  const slackActivity = ref<SlackThreadActivity[]>([])
  const slackState = ref<SourceState>({ loading: false, error: null, updatedAt: null })

  async function loadSlack() {
    slackState.value = { ...slackState.value, loading: true }
    try {
      const [threads, activity] = await Promise.all([
        invoke<SlackThreadSummary[]>('list_slack_threads', { projectId: null, query: null }),
        invoke<SlackThreadActivity[]>('list_slack_thread_activity', { projectId: null }),
      ])
      slackThreads.value = threads
      slackActivity.value = activity
      // Threads are fetched from Slack on demand: their age is the newest fetch.
      const fetched = threads.map((t) => parseTime(t.fetchedAt)).filter((t): t is number => t !== null)
      slackState.value = { loading: false, error: null, updatedAt: fetched.length ? Math.max(...fetched) : null }
    } catch (e) {
      slackState.value = { ...slackState.value, loading: false, error: String(e) }
    }
  }

  const slackRows = computed(() =>
    buildSlackRows(
      slackThreads.value.filter((t) => inProject(t.projectId, projectFilter.value) && t.fetchStatus !== 'error'),
      slackActivity.value,
      now.value,
    ),
  )
  const slackErrors = computed(() =>
    slackThreads.value.filter((t) => inProject(t.projectId, projectFilter.value) && t.fetchStatus === 'error').length,
  )

  // ── Todos ────────────────────────────────────────────────────────────────
  const todos = computed(() =>
    dashboardTodos(focus.items.filter((i) => inProject(i.todo.project_id, projectFilter.value)), now.value),
  )
  const todoState = computed<SourceState>(() => ({
    loading: focus.loading && !focus.loaded,
    error: focus.error,
    updatedAt: parseTime(focus.board?.now),
  }))

  // ── PRs ──────────────────────────────────────────────────────────────────
  const prs = computed(() =>
    rankPrs<PrInboxItem>(
      prInbox.items
        .filter((i) => inProject(i.project_id, projectFilter.value))
        // A live streaming status wins over the snapshot taken at fetch time.
        .map((i) => {
          const status = i.existing_run_id ? live.get(i.existing_run_id)?.status : undefined
          return status && status !== i.existing_run_status ? { ...i, existing_run_status: status } : i
        }),
      now.value,
      makeBotPredicate(prInbox.botLogins),
    ),
  )
  const prState = computed<SourceState>(() => ({
    loading: prInbox.loading,
    error: prInbox.error ?? (prInbox.warnings.length ? prInbox.warnings.map((w) => `${w.account_label}: ${w.message}`).join('; ') : null),
    updatedAt: parseTime(prInbox.lastRefreshedAt),
  }))

  // ── Project health ───────────────────────────────────────────────────────
  const scopeProjects = computed(() =>
    projects.projects.filter((p) => projectFilter.value === null || p.id === projectFilter.value),
  )
  const boardProjectIds = computed(() =>
    projects.projects.filter((p) => !!p.github_project_board_url).map((p) => p.id),
  )
  const healths = computed<ProjectHealth[]>(() =>
    scopeProjects.value
      .map((p) => {
        const state = radar.boards[p.id]
        return projectHealth(
          {
            projectId: p.id,
            projectName: p.name,
            board: p.github_project_board_url && state ? (state.board as unknown as HealthBoard) : null,
            error: radar.errors[p.id] ?? null,
            hasBoardUrl: !!p.github_project_board_url,
            gitlabOnly: gitlabOnly(p),
          },
          now.value,
          radar.thresholds,
          radar.doneRules,
        )
      })
      .sort(compareHealth),
  )
  const healthState = computed<SourceState>(() => {
    const ids = scopeProjects.value.filter((p) => !!p.github_project_board_url).map((p) => p.id)
    return {
      loading: ids.some((id) => radar.loading.has(id)),
      error: null,
      updatedAt: radar.oldestSavedAt(ids),
    }
  })

  // ── Running sessions ─────────────────────────────────────────────────────
  const activeRuns = ref<ActiveRun[]>([])
  const runState = ref<SourceState>({ loading: false, error: null, updatedAt: null })

  async function loadRuns() {
    runState.value = { ...runState.value, loading: true }
    try {
      activeRuns.value = await invoke<ActiveRun[]>('list_active_runs')
      runState.value = { loading: false, error: null, updatedAt: Date.now() }
    } catch (e) {
      runState.value = { ...runState.value, loading: false, error: String(e) }
    }
  }

  const runs = computed(() =>
    dashboardRuns(activeRuns.value.filter((r) => inProject(r.project_id, projectFilter.value)), now.value),
  )

  // ── By project ───────────────────────────────────────────────────────────
  /** Traffic-light rows: each project's health + what the other sources say about it. */
  const byProject = computed(() =>
    projectRows(healths.value, projectSignals({ slack: slackRows.value, todos: todos.value, prs: prs.value, runs: runs.value })),
  )

  // ── Loading + live updates ───────────────────────────────────────────────
  const refreshing = ref(false)
  /** When the last full refresh finished (null until the first one does). */
  const lastRefreshAt = ref<number | null>(null)

  async function refresh(force = false) {
    refreshing.value = true
    now.value = Date.now()
    try {
      if (projects.projects.length === 0) {
        try { await projects.fetchProjects() } catch { /* shown as empty widgets */ }
      }
      await Promise.allSettled([
        loadSlack(),
        loadRuns(),
        focus.load(),
        force ? prInbox.refresh() : prInbox.load(),
        radar.refresh(boardProjectIds.value, { force }),
      ])
      lastRefreshAt.value = Date.now()
    } finally {
      refreshing.value = false
    }
  }

  /** Sources whose last load failed (the header names them). */
  const failedSources = computed(() => {
    const out: AttentionKey[] = []
    if (slackState.value.error) out.push('slack')
    if (todoState.value.error) out.push('todos')
    if (prState.value.error) out.push('prs')
    if (scopeProjects.value.some((p) => p.github_project_board_url && radar.errors[p.id])) out.push('projects')
    if (runState.value.error) out.push('runs')
    return out
  })

  const timers = new Map<string, ReturnType<typeof setTimeout>>()
  // Set on unmount: no more debounced loads, and a listener whose registration
  // resolves afterwards is removed at once instead of outliving the screen.
  let disposed = false
  function debounced(key: string, fn: () => unknown) {
    if (disposed) return
    const prev = timers.get(key)
    if (prev) clearTimeout(prev)
    timers.set(key, setTimeout(() => {
      timers.delete(key)
      void fn()
    }, EVENT_DEBOUNCE_MS))
  }

  // Live sessions: the liveRuns store already follows run:activated / done /
  // permission events; refetch the metadata whenever its picture changes.
  const liveSignature = computed(() => {
    const parts: string[] = []
    live.sessions.forEach((s, id) => parts.push(`${id}:${s.status}:${s.permissionQueue.length}`))
    return parts.sort().join('|')
  })
  watch(liveSignature, () => debounced('runs', loadRuns))

  let unlisten: UnlistenFn[] = []
  async function follow(event: string, handler: () => void) {
    const off = await listen(event, handler)
    if (disposed) off()
    else unlisten.push(off)
  }
  onMounted(async () => {
    void refresh()
    try {
      await Promise.all([
        follow(SLACK_THREADS_CHANGED, () => debounced('slack', loadSlack)),
        follow(SLACK_CUSTOMERS_CHANGED, () => debounced('slack', loadSlack)),
        follow('runs:list_changed', () => debounced('runs', loadRuns)),
      ])
    } catch {
      /* outside the Tauri shell */
    }
  })
  onBeforeUnmount(() => {
    disposed = true
    for (const u of unlisten) u()
    unlisten = []
    for (const t of timers.values()) clearTimeout(t)
    timers.clear()
  })

  return {
    slackRows, slackErrors, slackState, slackThreadCount: computed(() => slackThreads.value.length),
    todos, todoState,
    prs, prState,
    healths, healthState, byProject,
    runs, runState,
    slackThreadById: (id: string) => slackThreads.value.find((t) => t.id === id) ?? null,
    refresh, refreshing, lastRefreshAt, failedSources, loadSlack, loadRuns,
  }
}
