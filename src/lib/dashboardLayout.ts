// Work Dashboard v2 ("read at a glance"): the order of the category cards,
// the groups inside each card, the plain-words reasons of a project and the
// per-project roll-up of the "By project" view.
//
// Every card ranks only within itself; the user picks the order of the cards.
// Pure logic (no Vue / Tauri) for node:test, on top of `dashboardSlack.ts`,
// `workDashboard.ts`, `prPriority.ts` and `projectHealth.ts`.
import { changedLines, SMALL_PR_LINES, type PrLite, type PrPriorityResult, type RankedPr } from './prPriority.ts'
import type { SlackDashboardRow, WaitLevel } from './dashboardSlack.ts'
import type { DashboardRun, DashboardTodo } from './workDashboard.ts'
import type { HealthLevel, ProjectHealth } from './projectHealth.ts'

// ── Card order ──────────────────────────────────────────────────────────────

export type CategoryKey = 'slack' | 'todos' | 'prs' | 'projects'

export const DEFAULT_CATEGORY_ORDER: CategoryKey[] = ['slack', 'todos', 'prs', 'projects']

/**
 * A saved order made whole: unknown / duplicate keys dropped, missing ones
 * appended in their default place. Anything unreadable gives the default.
 */
export function normalizeOrder(saved: unknown, defaults: readonly CategoryKey[] = DEFAULT_CATEGORY_ORDER): CategoryKey[] {
  const out: CategoryKey[] = []
  if (Array.isArray(saved)) {
    for (const k of saved) {
      if (defaults.includes(k as CategoryKey) && !out.includes(k as CategoryKey)) out.push(k as CategoryKey)
    }
  }
  for (const k of defaults) if (!out.includes(k)) out.push(k)
  return out
}

/** `key` first, the rest in their order (the keyboard path of drag & drop). */
export function moveToFront(order: CategoryKey[], key: CategoryKey): CategoryKey[] {
  return order.includes(key) ? [key, ...order.filter((k) => k !== key)] : [...order]
}

/** `key` dropped onto `target`: it takes the target's place, shifting the rest. */
export function moveTo(order: CategoryKey[], key: CategoryKey, target: CategoryKey): CategoryKey[] {
  const from = order.indexOf(key)
  const to = order.indexOf(target)
  if (from < 0 || to < 0 || from === to) return [...order]
  const out = order.filter((k) => k !== key)
  out.splice(to, 0, key)
  return out
}

// ── Groups within a card ────────────────────────────────────────────────────

export interface Group<K extends string, T> {
  key: K
  items: T[]
}

export interface FittedGroup<K extends string, T> {
  key: K
  /** All items of the group (for its count). */
  total: number
  shown: T[]
  /** Items left out to keep the card short (shown as one "+N more" line). */
  hidden: number
}

/**
 * Keep at most `max` rows over the groups, in group order: the first groups
 * fill up first, the rest of each group folds into its "+N more" line. Empty
 * groups are dropped; a group that gets no row at all keeps its header with
 * the folded count.
 */
export function fitGroups<K extends string, T>(groups: Group<K, T>[], max: number): FittedGroup<K, T>[] {
  let left = Math.max(0, max)
  const out: FittedGroup<K, T>[] = []
  for (const g of groups) {
    if (!g.items.length) continue
    const shown = g.items.slice(0, left)
    left -= shown.length
    out.push({ key: g.key, total: g.items.length, shown, hidden: g.items.length - shown.length })
  }
  return out
}

export type Level = 'bad' | 'warn' | 'ok' | 'none'

// Slack ─────────────────────────────────────────────────────────────────────

export interface SlackCard {
  /** Threads whose customer is waiting on the user. */
  waiting: SlackDashboardRow[]
  /** Waiting on the customer: nothing to answer now. */
  later: SlackDashboardRow[]
  /** Threads with new messages but no customer marked (one folded line). */
  unassignedNew: number
  level: Level
}

const WAIT_RANK: Record<WaitLevel, number> = { ok: 0, warn: 1, late: 2 }

export function slackCard(rows: SlackDashboardRow[]): SlackCard {
  const waiting = rows.filter((r) => r.label === 'waitingOnMe')
  const later = rows.filter((r) => r.label === 'waitingOnCustomer')
  const unassignedNew = rows.filter((r) => r.label === null && r.unread).length
  const worst = Math.max(-1, ...waiting.map((r) => WAIT_RANK[r.waitLevel]))
  const level: Level = worst === 2 ? 'bad' : worst >= 0 ? 'warn' : unassignedNew > 0 ? 'ok' : 'none'
  return { waiting, later, unassignedNew, level }
}

// Todos ─────────────────────────────────────────────────────────────────────

export type TodoGroupKey = 'overdue' | 'today' | 'upcoming'

export function todoCardGroups(todos: DashboardTodo[]): Group<TodoGroupKey, DashboardTodo>[] {
  return [
    { key: 'overdue', items: todos.filter((x) => x.group === 'overdue') },
    { key: 'today', items: todos.filter((x) => x.group === 'today') },
    { key: 'upcoming', items: todos.filter((x) => x.group === 'soon' || x.group === 'active') },
  ]
}

export function todoLevel(todos: DashboardTodo[]): Level {
  if (todos.some((x) => x.group === 'overdue')) return 'bad'
  if (todos.some((x) => x.group === 'today')) return 'warn'
  return todos.length ? 'ok' : 'none'
}

// PRs ───────────────────────────────────────────────────────────────────────

export type PrGroupKey = 'reReview' | 'toReview' | 'bots'

/** Changed lines from this many on make a "large" PR (under SMALL_PR_LINES = small). */
export const LARGE_PR_LINES = 500

export type PrSize = 'small' | 'medium' | 'large'

export function prSize(pr: PrLite): PrSize | null {
  const lines = changedLines(pr)
  if (lines === null) return null
  if (lines < SMALL_PR_LINES) return 'small'
  return lines >= LARGE_PR_LINES ? 'large' : 'medium'
}

/** How quick a review looks: small, CI passing, the user is the last reviewer (when known). */
export function quickReviewScore(r: RankedPr): number {
  return Number(r.small) + Number(r.ci === 'success') + Number(r.soleReviewer)
}

/**
 * Order inside a dashboard PR group — deliberately not by date (the PR Inbox
 * keeps `rankPrs`): drafts last, failing CI after the rest (it waits on its
 * author), then the quickest reviews first (`quickReviewScore`). The wait only
 * breaks ties, the longest first, then the PR number.
 */
export function compareCardPrs(a: RankedPr, b: RankedPr): number {
  return (
    Number(a.pr.draft) - Number(b.pr.draft) ||
    Number(a.ci === 'failure') - Number(b.ci === 'failure') ||
    quickReviewScore(b) - quickReviewScore(a) ||
    (b.waitDays ?? -1) - (a.waitDays ?? -1) ||
    a.pr.number - b.pr.number
  )
}

/**
 * New commits after the AI review → still to review (drafts last) → bots,
 * each group ordered by `compareCardPrs`.
 */
export function prCardGroups<T extends PrLite>(ranked: PrPriorityResult<T>): Group<PrGroupKey, RankedPr<T>>[] {
  const sorted = (list: RankedPr<T>[]) => [...list].sort(compareCardPrs)
  return [
    { key: 'reReview', items: sorted(ranked.rows.filter((r) => r.ai === 'updated' && !r.pr.draft)) },
    { key: 'toReview', items: sorted(ranked.rows.filter((r) => !(r.ai === 'updated' && !r.pr.draft))) },
    { key: 'bots', items: sorted(ranked.bots) },
  ]
}

export function prLevel<T extends PrLite>(ranked: PrPriorityResult<T>): Level {
  const open = ranked.rows.filter((r) => !r.pr.draft)
  if (open.some((r) => r.waitLevel === 'late')) return 'bad'
  if (open.some((r) => r.ai === 'updated' || r.waitLevel === 'warn')) return 'warn'
  return ranked.rows.length || ranked.bots.length ? 'ok' : 'none'
}

// Projects ──────────────────────────────────────────────────────────────────

/** One reason of a project's level, rendered as words by the UI. */
export type HealthReason =
  | { kind: 'issue', number: number, title: string, radar: string, daysLeft: number | null, url: string }
  | { kind: 'milestoneOverdue', title: string, lateDays: number }
  | { kind: 'milestoneLag', title: string, lagPct: number }
  | { kind: 'riskyIssues', count: number }

/**
 * Why a project is at risk, most telling first: its most urgent issue, its
 * worst milestone, then how many other issues carry a signal. At most `max`.
 */
export function healthReasons(h: ProjectHealth, max = 2): HealthReason[] {
  const out: HealthReason[] = []
  const top = h.topIssues[0]
  if (top) {
    out.push({ kind: 'issue', number: top.number, title: top.title, radar: top.radar, daysLeft: top.daysLeft, url: top.htmlUrl })
  }
  const m = h.milestones[0]
  if (m?.kind === 'overdue') out.push({ kind: 'milestoneOverdue', title: m.title, lateDays: m.lateDays ?? 0 })
  else if (m?.kind === 'lag') out.push({ kind: 'milestoneLag', title: m.title, lagPct: (m.elapsedPct ?? 0) - (m.closedPct ?? 0) })
  const others = h.riskyIssueCount - (top ? 1 : 0)
  if (others > 0) out.push({ kind: 'riskyIssues', count: others })
  return out.slice(0, max)
}

export interface ProjectCard {
  risk: ProjectHealth[]
  attention: ProjectHealth[]
  /** Could not be judged (no board, GitLab, error, loading…). */
  unknown: ProjectHealth[]
  ok: ProjectHealth[]
  level: Level
}

export function projectCard(healths: ProjectHealth[]): ProjectCard {
  const by = (l: HealthLevel) => healths.filter((h) => h.level === l)
  const risk = by('red')
  const attention = by('orange')
  return {
    risk,
    attention,
    unknown: by('gray'),
    ok: by('green'),
    // On track only when something was actually judged on track: all gray is "no verdict".
    level: risk.length ? 'bad' : attention.length ? 'warn' : healths.some((h) => h.level === 'green') ? 'ok' : 'none',
  }
}

/** Projects that could not be judged, one entry per reason (in first-seen order). */
export function unknownByReason(unknown: ProjectHealth[]): { reason: string, projects: ProjectHealth[] }[] {
  const map = new Map<string, ProjectHealth[]>()
  for (const h of unknown) {
    const key = h.reason ?? 'loading'
    const list = map.get(key)
    if (list) list.push(h)
    else map.set(key, [h])
  }
  return [...map].map(([reason, projects]) => ({ reason, projects }))
}

// ── "By project" view ───────────────────────────────────────────────────────

/** What the other sources say about one project. */
export interface ProjectSignals {
  customersWaiting: number
  /** The customer waiting the longest on the user, if any. */
  longestWait: { name: string, ms: number } | null
  prs: number
  overdueTodos: number
  /** Days late of the latest overdue todo. */
  worstTodoLateDays: number | null
  sessions: number
  sessionsWaiting: number
}

export function emptySignals(): ProjectSignals {
  return {
    customersWaiting: 0, longestWait: null, prs: 0, overdueTodos: 0, worstTodoLateDays: null, sessions: 0, sessionsWaiting: 0,
  }
}

export interface SignalSources<T extends PrLite & { project_id: string | null }> {
  slack: SlackDashboardRow[]
  todos: DashboardTodo[]
  prs: PrPriorityResult<T>
  runs: DashboardRun[]
}

/** Per project id: customers waiting, PRs to review, overdue todos, sessions. Unassigned items are left out. */
export function projectSignals<T extends PrLite & { project_id: string | null }>(
  src: SignalSources<T>,
): Map<string, ProjectSignals> {
  const map = new Map<string, ProjectSignals>()
  const of = (id: string | null | undefined): ProjectSignals | null => {
    if (!id) return null
    let s = map.get(id)
    if (!s) map.set(id, (s = emptySignals()))
    return s
  }
  for (const r of src.slack) {
    if (r.label !== 'waitingOnMe') continue
    const s = of(r.thread.projectId)
    if (!s) continue
    s.customersWaiting++
    const ms = r.waitingMs ?? 0
    if (!s.longestWait || ms > s.longestWait.ms) s.longestWait = { name: r.customer?.displayName ?? '', ms }
  }
  for (const x of src.todos) {
    if (x.group !== 'overdue') continue
    const s = of(x.item.todo.project_id)
    if (!s) continue
    s.overdueTodos++
    const late = -(x.daysLeft ?? 0)
    s.worstTodoLateDays = Math.max(s.worstTodoLateDays ?? 0, late)
  }
  for (const r of src.prs.rows) {
    if (r.pr.draft) continue
    const s = of(r.pr.project_id)
    if (s) s.prs++
  }
  for (const r of src.runs) {
    const s = of(r.run.project_id)
    if (!s) continue
    s.sessions++
    if (r.pending > 0) s.sessionsWaiting++
  }
  return map
}

export interface ProjectRow {
  health: ProjectHealth
  signals: ProjectSignals
}

const ROW_ORDER: Record<HealthLevel, number> = { red: 0, orange: 1, green: 2, gray: 3 }

/**
 * Red → orange → green → gray ("not enough data" last); within a level the
 * most overdue / risky first, then the most customers waiting, then by name.
 */
export function compareProjectRows(a: ProjectRow, b: ProjectRow): number {
  const ha = a.health
  const hb = b.health
  return (
    ROW_ORDER[ha.level] - ROW_ORDER[hb.level] ||
    hb.counts.overdue - ha.counts.overdue ||
    hb.riskyIssueCount - ha.riskyIssueCount ||
    hb.milestones.length - ha.milestones.length ||
    b.signals.customersWaiting - a.signals.customersWaiting ||
    ha.projectName.localeCompare(hb.projectName)
  )
}

export function projectRows(healths: ProjectHealth[], signals: Map<string, ProjectSignals>): ProjectRow[] {
  return healths
    .map((health) => ({ health, signals: signals.get(health.projectId) ?? emptySignals() }))
    .sort(compareProjectRows)
}

// ── Sessions ────────────────────────────────────────────────────────────────

/** The `StreamEntry` / `PermissionRequest` fields read here. */
interface EntryLite { kind: string, name?: string, displayName?: string, result?: unknown }
interface PromptLite { tool_name: string }

/**
 * What a live session is doing, from data the app already streams: the tool
 * it waits on permission for, else the tool still running (no result yet).
 * Null when nothing is known.
 */
export function sessionActivity(
  entries: readonly EntryLite[] | null | undefined,
  prompts: readonly PromptLite[] | null | undefined,
): { kind: 'permission' | 'tool', tool: string } | null {
  if (prompts?.length) return { kind: 'permission', tool: prompts[0].tool_name }
  if (!entries) return null
  for (let i = entries.length - 1; i >= 0; i--) {
    const e = entries[i]
    if (e.kind === 'result' || e.kind === 'user') return null
    if (e.kind === 'tool') return e.result === undefined ? { kind: 'tool', tool: e.displayName || e.name || '' } : null
  }
  return null
}
