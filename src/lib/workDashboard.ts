// Work Dashboard (the app's start screen): the todo and running-session
// widgets' ordering, the "needs you now" strip and the shared helpers
// (project filter, staleness). Slack / project health / PR ranking live in
// `dashboardSlack.ts`, `projectHealth.ts`, `prPriority.ts`.
//
// Pure logic (no Vue / Tauri) for node:test.
import { classifyTodo } from './focusRadar.ts'
import { daysUntil } from './deliveryRadar.ts'
import type { FocusItem, TodoStatus } from './focus.ts'

/** Rows shown per widget before "See all". */
export const WIDGET_ROWS = 5

// ── Todos ───────────────────────────────────────────────────────────────────

export type TodoGroup = 'overdue' | 'today' | 'soon' | 'active'

export const TODO_GROUPS: TodoGroup[] = ['overdue', 'today', 'soon', 'active']

/** A deadline within this many days (after today) is "due soon" here. */
export const TODO_SOON_DAYS = 3

export interface DashboardTodo {
  item: FocusItem
  group: TodoGroup
  /** Calendar days to the deadline (negative = late); null without one. */
  daysLeft: number | null
  status: TodoStatus
}

/**
 * The group of an open todo, or null when it does not need attention yet:
 * overdue → due today → due within `TODO_SOON_DAYS` → in progress / waiting.
 * Lateness follows the Todos window (`classifyTodo`: a waiting todo is not late).
 * Snoozed todos stay hidden until they wake up.
 */
export function todoGroup(item: FocusItem, now: number): TodoGroup | null {
  if (item.todo.done || item.bucket === 'done' || item.bucket === 'snoozed') return null
  const radar = classifyTodo(item, now)
  if (radar === 'overdue') return 'overdue'
  if (radar === 'today') return 'today'
  const left = daysUntil(item.todo.due_date, now)
  if (radar === 'due-soon' && left !== null && left <= TODO_SOON_DAYS) return 'soon'
  if (item.todo.status === 'doing' || item.todo.status === 'waiting') return 'active'
  return null
}

const GROUP_ORDER: Record<TodoGroup, number> = { overdue: 0, today: 1, soon: 2, active: 3 }

/** Grouped and ordered: by group, then the earliest deadline, then the backend score. */
export function dashboardTodos(items: FocusItem[], now: number): DashboardTodo[] {
  const out: DashboardTodo[] = []
  for (const item of items) {
    const group = todoGroup(item, now)
    if (group) out.push({ item, group, daysLeft: daysUntil(item.todo.due_date, now), status: item.todo.status })
  }
  return out.sort((a, b) =>
    GROUP_ORDER[a.group] - GROUP_ORDER[b.group] ||
    (a.daysLeft ?? Infinity) - (b.daysLeft ?? Infinity) ||
    (a.item.todo.status === 'doing' ? 0 : 1) - (b.item.todo.status === 'doing' ? 0 : 1) ||
    b.item.score - a.item.score,
  )
}

// ── Running sessions ────────────────────────────────────────────────────────

/** Mirrors the Rust `ActiveRun` (`list_active_runs`). */
export interface ActiveRun {
  run_id: string
  project_id: string
  project_name: string
  title: string | null
  run_type: string
  ref_number: number | null
  engine: string | null
  model: string | null
  status: string
  started_at: string | null
  created_at: string
  role: string | null
  conductor_run_id: string | null
  pending_permissions: number
  waiting_since: string | null
}

export interface DashboardRun {
  run: ActiveRun
  /** Own prompts + those of its workers. */
  pending: number
  /** Live workers folded into this conductor. */
  workerCount: number
  /** Since the start (running) or since the oldest prompt (waiting). */
  sinceMs: number | null
}

function ms(iso: string | null): number | null {
  if (!iso) return null
  const t = Date.parse(iso)
  return Number.isNaN(t) ? null : t
}

/**
 * Workers of a conductor that is in the list fold into its row (like the
 * sidebar dock), carrying their prompts with them. Awaiting permission first
 * (longest waiting first), then the longest running.
 */
export function dashboardRuns(runs: ActiveRun[], now: number): DashboardRun[] {
  const ids = new Set(runs.map((r) => r.run_id))
  const rows = new Map<string, DashboardRun>()
  const waitingSince = new Map<string, number>()
  const noteWait = (id: string, r: ActiveRun) => {
    const t = ms(r.waiting_since)
    if (r.pending_permissions > 0 && t !== null) {
      waitingSince.set(id, Math.min(waitingSince.get(id) ?? t, t))
    }
  }
  for (const r of runs) {
    if (r.conductor_run_id && ids.has(r.conductor_run_id)) continue
    rows.set(r.run_id, { run: r, pending: r.pending_permissions, workerCount: 0, sinceMs: null })
    noteWait(r.run_id, r)
  }
  for (const r of runs) {
    const parent = r.conductor_run_id ? rows.get(r.conductor_run_id) : undefined
    if (!parent) continue
    parent.workerCount++
    parent.pending += r.pending_permissions
    noteWait(parent.run.run_id, r)
  }
  const out = [...rows.values()]
  for (const row of out) {
    const since = row.pending > 0 ? waitingSince.get(row.run.run_id) ?? null : ms(row.run.started_at ?? row.run.created_at)
    row.sinceMs = since === null ? null : Math.max(0, now - since)
  }
  return out.sort((a, b) =>
    Number(b.pending > 0) - Number(a.pending > 0) ||
    (b.sinceMs ?? -1) - (a.sinceMs ?? -1) ||
    a.run.run_id.localeCompare(b.run.run_id),
  )
}

// ── Project filter / staleness ──────────────────────────────────────────────

/** `null` filter = every project; otherwise only rows of that project. */
export function inProject(projectId: string | null | undefined, filter: string | null): boolean {
  return filter === null || projectId === filter
}

/** Age of a snapshot in ms; null when unknown. */
export function ageMs(at: string | number | null | undefined, now: number): number | null {
  if (at === null || at === undefined) return null
  const t = typeof at === 'number' ? at : Date.parse(at)
  return Number.isNaN(t) ? null : Math.max(0, now - t)
}

/** A snapshot older than this is flagged as stale data. */
export const STALE_MS = 60 * 60_000

// ── "Needs you now" ─────────────────────────────────────────────────────────

export type AttentionKey = 'todos' | 'prs' | 'runs' | 'slack' | 'projects'

export interface AttentionItem {
  key: AttentionKey
  count: number
}

/**
 * The strip above the widgets, in display order, only what is non-zero:
 * overdue todos, PRs waiting for review, sessions awaiting permission,
 * customer threads waiting on me, projects at risk.
 */
export function attentionItems(counts: Record<AttentionKey, number>): AttentionItem[] {
  const order: AttentionKey[] = ['todos', 'prs', 'runs', 'slack', 'projects']
  return order.filter((k) => counts[k] > 0).map((key) => ({ key, count: counts[key] }))
}

/** Compact duration: `45m`, `3h`, `2d` (rounded down, ≥ 1 of its unit). */
export function shortDuration(msValue: number): { value: number, unit: 'm' | 'h' | 'd' } {
  const minutes = Math.max(0, Math.floor(msValue / 60_000))
  if (minutes < 60) return { value: Math.max(1, minutes), unit: 'm' }
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return { value: hours, unit: 'h' }
  return { value: Math.floor(hours / 24), unit: 'd' }
}
