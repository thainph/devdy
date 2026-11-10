// Delivery Radar: who is behind on their GitHub issues, across Devdy projects.
//
// Pure logic (no Vue / Tauri) so it can be unit-tested with node:test. The
// store flattens every project's `list_milestone_board` result into
// `RadarIssue`s; this module classifies each issue against the deadline taken
// from the project's linked GitHub Project board and rolls them up per
// assignee (or per project). Sub-issues are nested under their parent (epic /
// user story) when both land in the same group, and "done" depends on the
// assignee's role: every board status maps to a stage (dev → QA → done) — a
// developer is done once the issue reaches QA, a tester only once it is done.

/** The fields of the Rust `IssueRow` the radar reads. */
export interface RadarIssueRow {
  repo: string
  number: number
  title: string
  htmlUrl: string
  updatedAt: string
  assignees: string[]
  startDate: string | null
  /** Board Deadline field (YYYY-MM-DD). Milestone due dates are ignored. */
  deadline: string | null
  status: string | null
  statusColor: string | null
  state: string | null
  /** Parent issue (GitHub sub-issues); missing in snapshots from older versions. */
  parent?: RadarParentRef | null
  /** `deadline` comes from an ancestor (the issue has none of its own). */
  deadlineInherited?: boolean
}

export interface RadarParentRef {
  owner: string
  repo: string
  number: number
  title: string
  htmlUrl: string
}

export interface RadarIssue extends RadarIssueRow {
  projectId: string
  projectName: string
}

/** One status per issue, most urgent first (see `classifyIssue`). */
// GitHub Projects option colour → status dot (data palette, like the board).
const GH_STATUS_DOT: Record<string, string> = {
  GRAY: 'bg-slate-500', BLUE: 'bg-blue-500', GREEN: 'bg-emerald-500', YELLOW: 'bg-amber-500',
  ORANGE: 'bg-orange-500', RED: 'bg-red-500', PURPLE: 'bg-purple-500', PINK: 'bg-pink-500',
}

/** Tailwind background class of a board status dot. */
export function statusDotClass(color: string | null | undefined): string {
  return GH_STATUS_DOT[(color ?? '').toUpperCase()] ?? 'bg-muted-foreground/50'
}

export type RadarStatus = 'overdue' | 'due-soon' | 'not-started' | 'stale' | 'no-deadline' | 'on-track'

export const RADAR_STATUSES: RadarStatus[] = ['overdue', 'due-soon', 'not-started', 'stale', 'no-deadline', 'on-track']

const SEVERITY: Record<RadarStatus, number> = {
  overdue: 0,
  'due-soon': 1,
  'not-started': 2,
  stale: 3,
  'no-deadline': 4,
  'on-track': 5,
}

export interface RadarThresholds {
  /** A deadline today or within this many days is "due soon". */
  dueSoonDays: number
  /** No update on the issue for more than this many days is "stale". */
  staleDays: number
}

export const DEFAULT_THRESHOLDS: RadarThresholds = { dueSoonDays: 3, staleDays: 5 }

const DAY = 86_400_000
const NOT_STARTED = new Set(['', 'todo', 'to do', 'backlog', 'triage', 'ready', 'new', 'no status', 'chưa bắt đầu'])
// Default stage of a board column (until the user picks one): finished for
// everyone (released, done, cancelled, …), handed to QA (the developer's part
// is finished: QA, testing, staging, …), otherwise still in development.
const DONE_PATTERN = /releas|done|closed|complet|shipped|resolved|merged|cancel|won.?t ?fix|hoàn thành|xong|hủy/
const QA_PATTERN = /qa|test|uat|verif|staging|deploy/

/** Whose definition of done applies to an assignee. */
export type RadarRole = 'dev' | 'tester'

/** Where a board status sits: developer still on it → with the tester → finished. */
export type RadarStage = 'dev' | 'qa' | 'done'

export const RADAR_STAGES: RadarStage[] = ['dev', 'qa', 'done']

/** Stage picked per board status (lowercased name), shared by every project; absent = `defaultStage`. */
export type StageMap = Record<string, RadarStage | undefined>

/** Local midnight of a `YYYY-MM-DD` (or ISO) date; null when unparsable. */
export function dayStart(date: string | null | undefined): number | null {
  if (!date) return null
  const m = /^(\d{4})-(\d{2})-(\d{2})/.exec(date)
  if (!m) return null
  return new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3])).getTime()
}

function today(now: number): number {
  const d = new Date(now)
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime()
}

/** Whole days from today to the deadline: negative = late, 0 = due today. */
export function daysUntil(deadline: string | null, now: number): number | null {
  const dl = dayStart(deadline)
  if (dl === null) return null
  return Math.round((dl - today(now)) / DAY)
}

function normStatus(status: string | null | undefined): string {
  return (status ?? '').trim().toLowerCase()
}

/** Stage of a board status by name: Ready for QA, Testing → qa; Ready for release, Done → done. */
export function defaultStage(status: string | null | undefined): RadarStage {
  const s = normStatus(status)
  if (!s) return 'dev'
  if (DONE_PATTERN.test(s)) return 'done'
  if (QA_PATTERN.test(s)) return 'qa'
  return 'dev'
}

/** The user's pick for the status, else `defaultStage`. */
export function stageOf(status: string | null | undefined, stages?: StageMap | null): RadarStage {
  return stages?.[normStatus(status)] ?? defaultStage(status)
}

/**
 * Done from the role's point of view: closed on GitHub, or a status past the
 * role's stage — a developer is done from QA on, a tester only at done.
 */
export function isDone(issue: RadarIssueRow, stages?: StageMap | null, role: RadarRole = 'dev'): boolean {
  if (issue.state === 'CLOSED') return true
  const stage = stageOf(issue.status, stages)
  return role === 'tester' ? stage === 'done' : stage !== 'dev'
}

function isNotStarted(issue: RadarIssueRow): boolean {
  return NOT_STARTED.has((issue.status ?? '').trim().toLowerCase())
}

/**
 * The one status an open issue counts under, most urgent first:
 * overdue → due soon → not started (start date passed, still To do) → stale
 * (no update for too long) → no deadline → on track.
 */
export function classifyIssue(issue: RadarIssueRow, now: number, th: RadarThresholds): RadarStatus {
  const left = daysUntil(issue.deadline, now)
  if (left !== null && left < 0) return 'overdue'
  if (left !== null && left <= th.dueSoonDays) return 'due-soon'
  const start = dayStart(issue.startDate)
  if (start !== null && start <= today(now) && isNotStarted(issue)) return 'not-started'
  const updated = new Date(issue.updatedAt).getTime()
  if (!Number.isNaN(updated) && now - updated > th.staleDays * DAY) return 'stale'
  if (left === null) return 'no-deadline'
  return 'on-track'
}

export interface ClassifiedIssue extends RadarIssue {
  /** Unique across projects (repo names alone may collide). */
  key: string
  radar: RadarStatus
  /** Days to the deadline (negative = late); null without a deadline. */
  daysLeft: number | null
  /** Assignees whose part is not done yet (a dev drops out at Ready for QA, a tester stays). */
  activeAssignees: string[]
}

export interface DoneRules {
  /** Stage per board status (see `StageMap`). */
  stages?: StageMap
  /** Logins of the testers (case-insensitive); everyone else is a developer. */
  testers?: string[]
}

function issueKey(projectId: string, repo: string, number: number): string {
  return `${projectId}::${repo.toLowerCase()}#${number}`
}

/**
 * Classify the open work: an issue stays while at least one assignee still has
 * something to do on it (judged by their role); an unassigned one while it is
 * not done for a developer.
 */
export function classifyAll(
  issues: RadarIssue[],
  now: number,
  th: RadarThresholds,
  rules: DoneRules = {},
): ClassifiedIssue[] {
  const testers = new Set((rules.testers ?? []).map((l) => l.toLowerCase()))
  const seen = new Set<string>()
  const out: ClassifiedIssue[] = []
  for (const issue of issues) {
    const doneFor = (role: RadarRole) => isDone(issue, rules.stages, role)
    let activeAssignees: string[] = []
    if (issue.assignees.length) {
      activeAssignees = issue.assignees.filter((a) => !doneFor(testers.has(a.toLowerCase()) ? 'tester' : 'dev'))
      if (!activeAssignees.length) continue
    } else if (doneFor('dev')) {
      continue
    }
    const key = issueKey(issue.projectId, issue.repo, issue.number)
    if (seen.has(key)) continue
    seen.add(key)
    out.push({
      ...issue, key, radar: classifyIssue(issue, now, th), daysLeft: daysUntil(issue.deadline, now), activeAssignees,
    })
  }
  return out
}

export type RadarCounts = Record<RadarStatus, number>

export function emptyCounts(): RadarCounts {
  return { overdue: 0, 'due-soon': 0, 'not-started': 0, stale: 0, 'no-deadline': 0, 'on-track': 0 }
}

export function countByStatus(issues: ClassifiedIssue[]): RadarCounts {
  const counts = emptyCounts()
  for (const i of issues) counts[i.radar]++
  return counts
}

/** Most urgent first, then the latest deadline first, then least recently updated. */
export function compareIssues(a: ClassifiedIssue, b: ClassifiedIssue): number {
  const s = SEVERITY[a.radar] - SEVERITY[b.radar]
  if (s !== 0) return s
  const ad = a.daysLeft ?? Infinity
  const bd = b.daysLeft ?? Infinity
  if (ad !== bd) return ad - bd
  return a.updatedAt.localeCompare(b.updatedAt)
}

export type RadarGroupMode = 'person' | 'project'

export interface RadarGroup {
  /** Assignee login / project id; `null` = the unassigned bucket. */
  key: string | null
  label: string
  counts: RadarCounts
  total: number
  /** Days past the deadline of its latest issue; null when nothing is late. */
  worstLateDays: number | null
  /** Projects (person mode) or assignees (project mode) involved. */
  related: string[]
  issues: ClassifiedIssue[]
  /** `issues` as a tree: sub-issues right under their parent when it is in the group too. */
  rows: RadarRow[]
}

export interface RadarRow {
  issue: ClassifiedIssue
  /** 0 = top level; 1 = sub-issue of the row above it, … */
  depth: number
  hasChildren: boolean
}

/** Key of an issue's parent, in the same form as `ClassifiedIssue.key`. */
export function parentKey(issue: ClassifiedIssue): string | null {
  return issue.parent ? issueKey(issue.projectId, issue.parent.repo, issue.parent.number) : null
}

/**
 * Lay issues out as a tree: each sub-issue goes right under its parent (when
 * the parent is in the list), siblings and top-level rows ordered by the most
 * urgent issue of their subtree — so an epic whose task is late floats up with it.
 */
export function nestIssues(issues: ClassifiedIssue[]): RadarRow[] {
  const keys = new Set(issues.map((i) => i.key))
  const children = new Map<string, ClassifiedIssue[]>()
  const roots: ClassifiedIssue[] = []
  for (const issue of issues) {
    const pk = parentKey(issue)
    if (pk && pk !== issue.key && keys.has(pk)) {
      const list = children.get(pk)
      if (list) list.push(issue)
      else children.set(pk, [issue])
    } else {
      roots.push(issue)
    }
  }

  const lead = new Map<string, ClassifiedIssue>()
  const leadOf = (issue: ClassifiedIssue, path: Set<string>): ClassifiedIssue => {
    const cached = lead.get(issue.key)
    if (cached) return cached
    path.add(issue.key)
    let best = issue
    for (const c of children.get(issue.key) ?? []) {
      if (path.has(c.key)) continue
      const l = leadOf(c, path)
      if (compareIssues(l, best) < 0) best = l
    }
    path.delete(issue.key)
    lead.set(issue.key, best)
    return best
  }
  const bySubtree = (a: ClassifiedIssue, b: ClassifiedIssue) =>
    compareIssues(leadOf(a, new Set()), leadOf(b, new Set())) || compareIssues(a, b)

  const rows: RadarRow[] = []
  const placed = new Set<string>()
  const place = (issue: ClassifiedIssue, depth: number) => {
    if (placed.has(issue.key)) return
    placed.add(issue.key)
    const kids = [...(children.get(issue.key) ?? [])].sort(bySubtree)
    rows.push({ issue, depth, hasChildren: kids.length > 0 })
    for (const c of kids) place(c, depth + 1)
  }
  for (const r of [...roots].sort(bySubtree)) place(r, 0)
  // Issues caught in a parent cycle have no root: list them flat.
  for (const issue of issues) place(issue, 0)
  return rows
}

/**
 * Roll issues up per assignee (an issue with several assignees counts for each
 * of them; unassigned issues share one bucket, sorted last) or per project.
 * Groups with the most late work come first.
 */
export function buildGroups(issues: ClassifiedIssue[], mode: RadarGroupMode): RadarGroup[] {
  const groups = new Map<string, RadarGroup>()
  const UNASSIGNED = '\u0000unassigned'
  for (const issue of issues) {
    const targets: { id: string, key: string | null, label: string }[] =
      mode === 'project'
        ? [{ id: issue.projectId, key: issue.projectId, label: issue.projectName }]
        : issue.assignees.length
          ? issue.activeAssignees.map((a) => ({ id: a.toLowerCase(), key: a, label: a }))
          : [{ id: UNASSIGNED, key: null, label: '' }]
    for (const t of targets) {
      let g = groups.get(t.id)
      if (!g) {
        g = {
          key: t.key, label: t.label, counts: emptyCounts(), total: 0, worstLateDays: null, related: [], issues: [], rows: [],
        }
        groups.set(t.id, g)
      }
      g.issues.push(issue)
      g.counts[issue.radar]++
      g.total++
      if (issue.daysLeft !== null && issue.daysLeft < 0) {
        g.worstLateDays = Math.max(g.worstLateDays ?? 0, -issue.daysLeft)
      }
      const related = mode === 'project' ? issue.activeAssignees : [issue.projectName]
      for (const r of related) if (!g.related.includes(r)) g.related.push(r)
    }
  }
  const list = [...groups.values()]
  for (const g of list) {
    g.issues.sort(compareIssues)
    g.rows = nestIssues(g.issues)
    g.related.sort((a, b) => a.localeCompare(b))
  }
  return list.sort((a, b) =>
    (a.key === null ? 1 : 0) - (b.key === null ? 1 : 0) ||
    b.counts.overdue - a.counts.overdue ||
    (b.worstLateDays ?? 0) - (a.worstLateDays ?? 0) ||
    b.counts['due-soon'] - a.counts['due-soon'] ||
    b.counts['not-started'] + b.counts.stale - (a.counts['not-started'] + a.counts.stale) ||
    b.total - a.total ||
    a.label.localeCompare(b.label),
  )
}

/** Keep only issues of `status` (when set) whose title/repo/number/assignee matches `query`. */
export function filterIssues(
  issues: ClassifiedIssue[],
  opts: { status?: RadarStatus | null, query?: string },
): ClassifiedIssue[] {
  const q = (opts.query ?? '').trim().toLowerCase()
  return issues.filter((i) => {
    if (opts.status && i.radar !== opts.status) return false
    if (!q) return true
    const num = q.replace(/^#/, '')
    return (
      i.title.toLowerCase().includes(q) ||
      !!i.parent?.title.toLowerCase().includes(q) ||
      i.repo.toLowerCase().includes(q) ||
      String(i.number) === num ||
      i.assignees.some((a) => a.toLowerCase().includes(q))
    )
  })
}
