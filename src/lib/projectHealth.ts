// Work Dashboard — "projects at risk of slipping": one health level per
// project from its Delivery Radar board (`list_milestone_board`).
//
// Pure logic (no Vue / Tauri) for node:test. Issues are classified exactly like
// the radar (`classifyAll`); milestones add the radar's *estimated* milestone
// signal (`milestoneRisk`: past due with issues open, or behind schedule by
// more than `milestoneLagPct`).
//
// Levels: red = an overdue issue or milestone; orange = an issue due soon /
// not started / stale, or a milestone behind schedule; green = no signal;
// gray = nothing to judge by (no board, GitLab only, fetch error, or a
// truncated board with no signal) — always with the reason.
import {
  classifyAll, compareMilestoneRisks, countByStatus, compareIssues, dayStart, milestoneRisk,
  type ClassifiedIssue, type DoneRules, type MilestoneRisk, type RadarCounts, type RadarIssue, type RadarMilestone,
  type RadarThresholds,
} from './deliveryRadar.ts'

export type { MilestoneRisk } from './deliveryRadar.ts'

/** The `MilestoneBoard` fields read here. */
export interface HealthBoard {
  milestones: RadarMilestone[]
  fetchedAt: string
  boardLinked: boolean
  truncated: boolean
  skippedNonGithub: boolean
}

export interface HealthInput {
  projectId: string
  projectName: string
  /** Cached / fetched board, if any. */
  board: HealthBoard | null
  /** Last fetch error (the cached board, if any, is still judged). */
  error: string | null
  /** The project has a GitHub Project board configured. */
  hasBoardUrl: boolean
  /** The project only uses GitLab (the radar does not cover it). */
  gitlabOnly: boolean
}

export type HealthLevel = 'red' | 'orange' | 'green' | 'gray'
export type GrayReason = 'no-board' | 'gitlab' | 'error' | 'truncated' | 'loading'

export interface ProjectHealth {
  projectId: string
  projectName: string
  level: HealthLevel
  /** Why it is gray, or a caveat on a red/orange/green verdict (truncated, error). */
  reason: GrayReason | null
  counts: RadarCounts
  /** Open issues with a risk signal (overdue / due soon / not started / stale). */
  riskyIssueCount: number
  /** The 3 latest issues, most urgent first. */
  topIssues: ClassifiedIssue[]
  milestones: MilestoneRisk[]
  /** Progress of the current milestone (see `currentMilestoneProgress`), if any. */
  progress: MilestoneProgress | null
  /** When the board was fetched (ISO), if any. */
  fetchedAt: string | null
}

/** Done vs elapsed time of one milestone (an estimate, like `milestoneRisk`). */
export interface MilestoneProgress {
  title: string
  dueOn: string
  /** Share of its issues closed, 0–100. */
  closedPct: number
  /** Share of the planned time elapsed, 0–100; null without a start date. */
  elapsedPct: number | null
}

const DAY = 86_400_000

/**
 * The milestone being worked on now — the earliest due one with open issues
 * and GitHub's totals — with its closed share and, when its issues carry a
 * board Start date (the stand-in start, as in `milestoneRisk`), the share of
 * time elapsed until the end of its due day.
 */
export function currentMilestoneProgress(milestones: RadarMilestone[], now: number): MilestoneProgress | null {
  const due = (m: RadarMilestone) => dayStart(m.dueOn)
  const current = milestones
    .filter((m) => due(m) !== null && m.openCount > 0 && m.countsKnown !== false)
    .sort((a, b) => due(a)! - due(b)! || a.title.localeCompare(b.title))[0]
  if (!current) return null
  const total = current.openCount + current.closedCount
  const closedPct = total > 0 ? Math.round((current.closedCount / total) * 100) : 0
  const starts = (current.issues ?? []).map((i) => dayStart(i.startDate)).filter((s): s is number => s !== null)
  const end = due(current)! + DAY
  let elapsedPct: number | null = null
  if (starts.length) {
    const start = Math.min(...starts)
    if (start < end) elapsedPct = Math.round(Math.min(100, Math.max(0, ((now - start) / (end - start)) * 100)))
  }
  return { title: current.title, dueOn: current.dueOn!, closedPct, elapsedPct }
}

const RISKY = new Set(['overdue', 'due-soon', 'not-started', 'stale'])
function flatIssues(input: HealthInput): RadarIssue[] {
  const out: RadarIssue[] = []
  for (const g of input.board?.milestones ?? []) {
    for (const i of g.issues) out.push({ ...i, projectId: input.projectId, projectName: input.projectName })
  }
  return out
}

export function projectHealth(
  input: HealthInput,
  now: number,
  th: RadarThresholds,
  rules: DoneRules = {},
): ProjectHealth {
  const board = input.board
  const base = {
    projectId: input.projectId,
    projectName: input.projectName,
    fetchedAt: board?.fetchedAt ?? null,
  }
  if (!board) {
    const reason: GrayReason = input.gitlabOnly
      ? 'gitlab'
      : !input.hasBoardUrl ? 'no-board' : input.error ? 'error' : 'loading'
    return {
      ...base, level: 'gray', reason, counts: countByStatus([]), riskyIssueCount: 0, topIssues: [], milestones: [],
      progress: null,
    }
  }

  // Without a linked board there are no deadlines: issue signals would only be noise.
  const issues = board.boardLinked ? classifyAll(flatIssues(input), now, th, rules) : []
  const risky = issues.filter((i) => RISKY.has(i.radar)).sort(compareIssues)
  const counts = countByStatus(issues)
  const milestones = board.milestones
    .map((m) => milestoneRisk(m, now, th.milestoneLagPct))
    .filter((m): m is MilestoneRisk => m !== null)
    .sort(compareMilestoneRisks)

  let level: HealthLevel = 'green'
  if (counts.overdue > 0 || milestones.some((m) => m.kind === 'overdue')) level = 'red'
  else if (risky.length > 0 || milestones.length > 0) level = 'orange'

  let reason: GrayReason | null = null
  if (input.error) reason = 'error'
  else if (!board.boardLinked) reason = board.skippedNonGithub && board.milestones.length === 0 ? 'gitlab' : 'no-board'
  else if (board.truncated) reason = 'truncated'
  // "No data" is not "no risk": a clean verdict we cannot stand behind is gray.
  if (level === 'green' && reason) level = 'gray'

  return {
    ...base,
    level,
    reason,
    counts,
    riskyIssueCount: risky.length,
    topIssues: risky.slice(0, 3),
    milestones,
    progress: board.boardLinked ? currentMilestoneProgress(board.milestones, now) : null,
  }
}

const LEVEL_ORDER: Record<HealthLevel, number> = { red: 0, orange: 1, gray: 2, green: 3 }

/** Red → orange → gray → green; then the most overdue / risky issues first. */
export function compareHealth(a: ProjectHealth, b: ProjectHealth): number {
  return (
    LEVEL_ORDER[a.level] - LEVEL_ORDER[b.level] ||
    b.counts.overdue - a.counts.overdue ||
    b.riskyIssueCount - a.riskyIssueCount ||
    b.milestones.length - a.milestones.length ||
    a.projectName.localeCompare(b.projectName)
  )
}

export function isAtRisk(h: ProjectHealth): boolean {
  return h.level === 'red' || h.level === 'orange'
}
