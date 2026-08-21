import { defineStore } from 'pinia'
import { invoke } from '@/lib/tauri'
import { ref, computed } from 'vue'

/** Mirrors the Rust `LabelRow`. */
export interface LabelRow {
  name: string
  color: string
}

/** Mirrors the Rust `IssueRow` (camelCase via serde rename_all). */
export interface IssueRow {
  repo: string
  number: number
  title: string
  htmlUrl: string
  createdAt: string
  updatedAt: string
  comments: number
  assignees: string[]
  labels: LabelRow[]
  milestoneTitle: string | null
  milestoneDueOn: string | null
  startDate: string | null
  deadline: string | null
  status: string | null
  statusColor: string | null
  state: string | null
  closedAt: string | null
}

export interface MilestoneGroup {
  title: string
  dueOn: string | null
  repos: string[]
  openCount: number
  closedCount: number
  issues: IssueRow[]
}

export interface MilestoneBoard {
  milestones: MilestoneGroup[]
  fetchedAt: string
  boardLinked: boolean
  truncated: boolean
  skippedNonGithub: boolean
}

export type IssueSlowStatus = 'overdue' | 'at-risk' | 'stale' | 'not-started' | 'ok'

const DAY = 86_400_000
const NOT_STARTED = new Set(['', 'todo', 'to do', 'backlog', 'triage', 'no status', 'chưa bắt đầu'])
const DONE = new Set(['done', 'closed', 'complete', 'completed', 'hoàn thành', 'shipped'])

// Cross-window persistence. Each Tauri window is its own webview/JS context, so
// the in-memory Map cache below is NOT shared — a freshly opened Gantt pop-out
// starts empty. localStorage IS shared across same-origin windows, so we mirror
// each fetched board there: a new window (or an app restart) can hydrate the
// chart instantly from the last snapshot instead of blocking on a full GitHub
// round-trip, then revalidate in the background when the snapshot is stale.
const PERSIST_PREFIX = 'devdy:ganttBoard:'
// Snapshots newer than this are served as-is with no network call at all.
const REVALIDATE_MS = 5 * 60 * 1000

interface PersistedBoard {
  savedAt: number
  board: MilestoneBoard
}

function readPersisted(projectId: string): PersistedBoard | null {
  try {
    const raw = localStorage.getItem(PERSIST_PREFIX + projectId)
    if (!raw) return null
    const parsed = JSON.parse(raw) as PersistedBoard
    if (!parsed?.board?.milestones) return null
    return parsed
  } catch {
    return null
  }
}

function writePersisted(projectId: string, board: MilestoneBoard) {
  try {
    const payload: PersistedBoard = { savedAt: Date.now(), board }
    localStorage.setItem(PERSIST_PREFIX + projectId, JSON.stringify(payload))
  } catch {
    // Quota exceeded / serialization issue — persistence is best-effort.
  }
}

export const useProjectIssuesStore = defineStore('projectIssues', () => {
  const board = ref<MilestoneBoard | null>(null)
  const loading = ref(false)
  const error = ref<string | null>(null)
  const lastRefreshedAt = ref<string | null>(null)
  const currentProjectId = ref<string | null>(null)
  // Client-side thresholds so the user can retune "slow" without a round-trip.
  const staleDays = ref(14)
  const atRiskDays = ref(7)

  // Per-project cache so switching back to an already-loaded project is instant.
  const cache = new Map<string, MilestoneBoard>()

  // On-demand per-milestone fetch of ALL issues (open + closed). Keyed by
  // `${projectId}::${milestoneTitleLower}`. The main board only carries open
  // issues; a milestone is loaded here only when the user expands it.
  const milestoneClosed = ref<Record<string, IssueRow[]>>({})
  const milestoneClosedLoading = ref<Set<string>>(new Set())
  const milestoneClosedError = ref<Record<string, string>>({})

  function closedKey(projectId: string, title: string): string {
    return `${projectId}::${title.toLowerCase()}`
  }
  function closedIssuesFor(projectId: string, title: string): IssueRow[] | null {
    return milestoneClosed.value[closedKey(projectId, title)] ?? null
  }
  function isLoadingClosed(projectId: string, title: string): boolean {
    return milestoneClosedLoading.value.has(closedKey(projectId, title))
  }

  async function loadMilestoneIssues(projectId: string, title: string) {
    const key = closedKey(projectId, title)
    if (milestoneClosedLoading.value.has(key)) return
    const startLoad = new Set(milestoneClosedLoading.value)
    startLoad.add(key)
    milestoneClosedLoading.value = startLoad
    try {
      const issues = await invoke<IssueRow[]>('list_milestone_issues', {
        projectId,
        milestoneTitle: title,
      })
      milestoneClosed.value = { ...milestoneClosed.value, [key]: issues }
      const { [key]: _removed, ...rest } = milestoneClosedError.value
      milestoneClosedError.value = rest
    } catch (e) {
      milestoneClosedError.value = { ...milestoneClosedError.value, [key]: String(e) }
    } finally {
      const endLoad = new Set(milestoneClosedLoading.value)
      endLoad.delete(key)
      milestoneClosedLoading.value = endLoad
    }
  }

  async function refresh(projectId: string, opts: { force?: boolean } = {}) {
    currentProjectId.value = projectId
    error.value = null

    // Seed from the fastest source available: this session's in-memory cache
    // first, then the cross-window localStorage snapshot. This is what lets a
    // freshly opened Gantt window (or a cold app start) paint the chart at once
    // using data the main window already fetched, instead of a blank skeleton.
    const mem = cache.get(projectId)
    const persisted = mem ? null : readPersisted(projectId)
    const seed = mem ?? persisted?.board ?? null
    board.value = seed
    if (persisted && !mem) cache.set(projectId, persisted.board)
    if (persisted?.savedAt) lastRefreshedAt.value = new Date(persisted.savedAt).toISOString()

    // Serve cached data without any network round-trip when it's fresh enough:
    // in-memory always counts as fresh (same session); a persisted snapshot only
    // within REVALIDATE_MS. `force` (the Refresh button) always bypasses this.
    const persistedFresh = !!persisted && Date.now() - persisted.savedAt < REVALIDATE_MS
    if (seed && !opts.force && (!!mem || persistedFresh)) {
      loading.value = false
      return
    }

    // Otherwise fetch. Show the full skeleton when there's nothing to show; a
    // background revalidate with seed data on screen stays silent. But an explicit
    // `force` (the Refresh button) always flips `loading` so the button gets a
    // spinner/disabled state — otherwise a manual refresh gives no feedback.
    loading.value = !seed || !!opts.force
    try {
      const result = await invoke<MilestoneBoard>('list_milestone_board', { projectId })
      cache.set(projectId, result)
      writePersisted(projectId, result)
      // Only apply if this is still the active project (guards fast switching).
      if (currentProjectId.value === projectId) {
        board.value = result
        lastRefreshedAt.value = new Date().toISOString()
      }
    } catch (e) {
      if (currentProjectId.value === projectId) {
        // Keep any seed data on screen; only surface the error when we have
        // nothing else to show (a silent background revalidate shouldn't wipe
        // a perfectly good cached chart just because the network hiccuped).
        if (!seed) {
          error.value = String(e)
          board.value = null
        }
      }
    } finally {
      if (currentProjectId.value === projectId) loading.value = false
    }
  }

  /** Effective deadline: board field wins, otherwise the milestone due date. */
  function effectiveDeadline(issue: IssueRow): string | null {
    return issue.deadline ?? issue.milestoneDueOn ?? null
  }

  function isDone(issue: IssueRow): boolean {
    if (issue.state === 'CLOSED') return true
    return DONE.has((issue.status ?? '').toLowerCase())
  }

  function issueStatus(issue: IssueRow): IssueSlowStatus {
    if (isDone(issue)) return 'ok'
    const now = Date.now()
    const dl = effectiveDeadline(issue)
    const dlTime = dl ? new Date(dl).getTime() : null
    if (dlTime !== null && dlTime < now) return 'overdue'
    if (dlTime !== null && dlTime - now <= atRiskDays.value * DAY) return 'at-risk'
    const upd = new Date(issue.updatedAt).getTime()
    if (!Number.isNaN(upd) && now - upd > staleDays.value * DAY) return 'stale'
    if (issue.startDate) {
      const st = new Date(issue.startDate).getTime()
      if (!Number.isNaN(st) && st < now && NOT_STARTED.has((issue.status ?? '').toLowerCase())) {
        return 'not-started'
      }
    }
    return 'ok'
  }

  const summary = computed(() => {
    let open = 0, overdue = 0, stale = 0, atRisk = 0, notStarted = 0
    for (const g of board.value?.milestones ?? []) {
      for (const it of g.issues) {
        open++
        switch (issueStatus(it)) {
          case 'overdue': overdue++; break
          case 'stale': stale++; break
          case 'at-risk': atRisk++; break
          case 'not-started': notStarted++; break
        }
      }
    }
    return { open, overdue, stale, atRisk, notStarted }
  })

  return {
    board, loading, error, lastRefreshedAt, currentProjectId,
    staleDays, atRiskDays,
    refresh, effectiveDeadline, isDone, issueStatus, summary,
    milestoneClosed, milestoneClosedLoading, milestoneClosedError,
    loadMilestoneIssues, closedIssuesFor, isLoadingClosed,
  }
})
