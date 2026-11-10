import { defineStore } from 'pinia'
import { computed, ref, watch } from 'vue'
import { invoke } from '@/lib/tauri'
import {
  DEFAULT_THRESHOLDS, type DoneRules, type RadarIssue, type RadarIssueRow, type RadarRole, type RadarStage,
  type RadarThresholds, type StageMap,
} from '@/lib/deliveryRadar'

/** Mirrors the Rust `MilestoneBoard` (only what the radar reads). */
interface MilestoneBoard {
  milestones: { issues: RadarIssueRow[] }[]
  fetchedAt: string
  boardLinked: boolean
  truncated: boolean
  skippedNonGithub: boolean
  /** Status options of the board, in board order (missing in old snapshots). */
  statusOptions?: { name: string, color: string | null }[]
}

export interface StatusOptionInfo {
  name: string
  color: string | null
  /** Open issues currently in this status. */
  count: number
  /** Projects whose board has this status. */
  projectIds: string[]
}

export interface ProjectBoardState {
  board: MilestoneBoard
  savedAt: number
}

// Cross-window persistence: the radar lives in its own OS window (own JS
// context), so each project's last board is mirrored to localStorage — a fresh
// window or a cold start paints at once, then revalidates in the background.
const PERSIST_PREFIX = 'devdy:radarBoard:'
const THRESHOLDS_KEY = 'devdy:radarThresholds'
const STAGES_KEY = 'devdy:radarStatusStages'
const TESTERS_KEY = 'devdy:radarTesters'
// Per-project done lists of earlier versions, superseded by the shared stages.
const LEGACY_KEYS = ['devdy:radarDoneStatuses', 'devdy:radarQaDoneStatuses']
// Snapshots newer than this are served as-is with no network call at all.
const REVALIDATE_MS = 5 * 60 * 1000
// Each project already fetches its repos + board in parallel on the Rust side;
// cap how many projects hit GitHub at once.
const CONCURRENCY = 3

function readPersisted(projectId: string): ProjectBoardState | null {
  try {
    const raw = localStorage.getItem(PERSIST_PREFIX + projectId)
    if (!raw) return null
    const parsed = JSON.parse(raw) as ProjectBoardState
    return parsed?.board?.milestones ? parsed : null
  } catch {
    return null
  }
}

function writePersisted(projectId: string, state: ProjectBoardState) {
  try {
    localStorage.setItem(PERSIST_PREFIX + projectId, JSON.stringify(state))
  } catch {
    // Quota exceeded / serialization issue — persistence is best-effort.
  }
}

function readThresholds(): RadarThresholds {
  try {
    const parsed = JSON.parse(localStorage.getItem(THRESHOLDS_KEY) ?? 'null') as Partial<RadarThresholds> | null
    return { ...DEFAULT_THRESHOLDS, ...(parsed ?? {}) }
  } catch {
    return { ...DEFAULT_THRESHOLDS }
  }
}

function readStages(): StageMap {
  try {
    for (const k of LEGACY_KEYS) localStorage.removeItem(k)
    const parsed = JSON.parse(localStorage.getItem(STAGES_KEY) ?? 'null') as StageMap | null
    return parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? parsed : {}
  } catch {
    return {}
  }
}

function readTesters(): string[] {
  try {
    const parsed = JSON.parse(localStorage.getItem(TESTERS_KEY) ?? 'null') as unknown
    return Array.isArray(parsed) ? parsed.filter((l): l is string => typeof l === 'string') : []
  } catch {
    return []
  }
}

function persist(key: string, value: unknown) {
  try { localStorage.setItem(key, JSON.stringify(value)) } catch { /* best-effort */ }
}

export const useDeliveryRadarStore = defineStore('deliveryRadar', () => {
  const boards = ref<Record<string, ProjectBoardState>>({})
  const errors = ref<Record<string, string>>({})
  const loading = ref<Set<string>>(new Set())
  const thresholds = ref<RadarThresholds>(readThresholds())
  // Stage the user picked per board status (lowercased name), shared by every
  // project; a status absent here uses `defaultStage`.
  const stages = ref<StageMap>(readStages())
  // GitHub logins (lowercased) of the testers; every other assignee is a developer.
  const testers = ref<string[]>(readTesters())

  watch(thresholds, (v) => persist(THRESHOLDS_KEY, v), { deep: true })
  watch(stages, (v) => persist(STAGES_KEY, v), { deep: true })
  watch(testers, (v) => persist(TESTERS_KEY, v), { deep: true })

  const doneRules = computed<DoneRules>(() => ({ stages: stages.value, testers: testers.value }))

  /** Overrides keyed by status name; anything not listed falls back to the default. */
  function setStages(next: Record<string, RadarStage>) {
    const out: StageMap = {}
    for (const [name, stage] of Object.entries(next)) out[name.trim().toLowerCase()] = stage
    stages.value = out
  }

  function roleOf(login: string): RadarRole {
    return testers.value.includes(login.toLowerCase()) ? 'tester' : 'dev'
  }

  function setTesters(logins: string[]) {
    testers.value = [...new Set(logins.map((l) => l.toLowerCase()))].sort()
  }

  /** Every assignee seen on the given projects' issues (for the role picker). */
  function assigneesFor(projectIds: string[]): string[] {
    const byKey = new Map<string, string>()
    for (const id of projectIds) {
      for (const g of boards.value[id]?.board.milestones ?? []) {
        for (const i of g.issues) for (const a of i.assignees) byKey.set(a.toLowerCase(), a)
      }
    }
    return [...byKey.values()].sort((a, b) => a.localeCompare(b))
  }

  /**
   * The Status options of the projects' boards merged by name (case-insensitive),
   * in board order, plus any status seen on their issues (renamed / removed columns).
   */
  function statusOptionsFor(projectIds: string[]): StatusOptionInfo[] {
    const byKey = new Map<string, StatusOptionInfo>()
    const add = (projectId: string, name: string, color: string | null) => {
      let info = byKey.get(name.toLowerCase())
      if (!info) {
        info = { name, color, count: 0, projectIds: [] }
        byKey.set(name.toLowerCase(), info)
      }
      if (!info.color) info.color = color
      if (!info.projectIds.includes(projectId)) info.projectIds.push(projectId)
      return info
    }
    for (const id of projectIds) {
      const board = boards.value[id]?.board
      for (const o of board?.statusOptions ?? []) {
        // Snapshots from before colours were returned hold plain names.
        const opt = typeof o === 'string' ? { name: o as string, color: null } : o
        add(id, opt.name, opt.color)
      }
      for (const g of board?.milestones ?? []) {
        for (const i of g.issues) {
          const name = i.status?.trim()
          if (name) add(id, name, i.statusColor).count++
        }
      }
    }
    return [...byKey.values()]
  }

  function setLoading(projectId: string, on: boolean) {
    const next = new Set(loading.value)
    if (on) next.add(projectId)
    else next.delete(projectId)
    loading.value = next
  }

  async function refreshOne(projectId: string, force: boolean) {
    let state = boards.value[projectId] ?? readPersisted(projectId)
    if (state && !boards.value[projectId]) boards.value = { ...boards.value, [projectId]: state }
    if (state && !force && Date.now() - state.savedAt < REVALIDATE_MS) return

    setLoading(projectId, true)
    try {
      const board = await invoke<MilestoneBoard>('list_milestone_board', { projectId })
      state = { board, savedAt: Date.now() }
      boards.value = { ...boards.value, [projectId]: state }
      writePersisted(projectId, state)
      if (projectId in errors.value) {
        const rest = { ...errors.value }
        delete rest[projectId]
        errors.value = rest
      }
    } catch (e) {
      // Keep the cached board on screen; the error is shown next to it.
      errors.value = { ...errors.value, [projectId]: String(e) }
    } finally {
      setLoading(projectId, false)
    }
  }

  /** Load every project (cache first); `force` (Refresh button) always refetches. */
  async function refresh(projectIds: string[], opts: { force?: boolean } = {}) {
    const queue = [...projectIds]
    const worker = async () => {
      for (let id = queue.shift(); id; id = queue.shift()) await refreshOne(id, !!opts.force)
    }
    await Promise.all(Array.from({ length: Math.min(CONCURRENCY, queue.length) }, worker))
  }

  /** Open issues of the given projects, tagged with their project. */
  function issuesFor(projects: { id: string, name: string }[]): RadarIssue[] {
    const out: RadarIssue[] = []
    for (const p of projects) {
      for (const g of boards.value[p.id]?.board.milestones ?? []) {
        for (const issue of g.issues) out.push({ ...issue, projectId: p.id, projectName: p.name })
      }
    }
    return out
  }

  /** Oldest snapshot among the loaded projects (what "updated …" should show). */
  function oldestSavedAt(projectIds: string[]): number | null {
    const times = projectIds.map((id) => boards.value[id]?.savedAt).filter((t): t is number => !!t)
    return times.length ? Math.min(...times) : null
  }

  const isLoading = computed(() => loading.value.size > 0)

  return {
    boards, errors, loading, isLoading, thresholds, stages, testers, doneRules,
    refresh, issuesFor, oldestSavedAt, setStages, statusOptionsFor, roleOf, setTesters, assigneesFor,
  }
})
