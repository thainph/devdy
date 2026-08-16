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

export const useProjectIssuesStore = defineStore('projectIssues', () => {
  const board = ref<MilestoneBoard | null>(null)
  const loading = ref(false)
  const error = ref<string | null>(null)
  const lastRefreshedAt = ref<string | null>(null)
  const currentProjectId = ref<string | null>(null)
  // Client-side thresholds so the user can retune "slow" without a round-trip.
  const staleDays = ref(14)
  const atRiskDays = ref(7)

  async function refresh(projectId: string) {
    loading.value = true
    error.value = null
    currentProjectId.value = projectId
    try {
      board.value = await invoke<MilestoneBoard>('list_milestone_board', { projectId })
      lastRefreshedAt.value = new Date().toISOString()
    } catch (e) {
      error.value = String(e)
      board.value = null
    } finally {
      loading.value = false
    }
  }

  /** Effective deadline: board field wins, otherwise the milestone due date. */
  function effectiveDeadline(issue: IssueRow): string | null {
    return issue.deadline ?? issue.milestoneDueOn ?? null
  }

  function isDone(issue: IssueRow): boolean {
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
  }
})
