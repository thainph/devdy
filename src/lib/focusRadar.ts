// Focus radar: which of my open todos are late, per project — the Focus
// screen's only view. Pure logic (no Vue / Tauri) for node:test.
//
// Lateness is computed here from the todo's deadline rather than taken from
// the backend bucket: the bucket ranks `doing` / `waiting` above time, so a
// todo in progress past its deadline would otherwise not show as overdue.
import { daysUntil } from './deliveryRadar.ts'
import type { FocusItem } from './focus.ts'

/** One status per todo, most urgent first. */
export type FocusRadarStatus = 'overdue' | 'today' | 'due-soon' | 'waiting' | 'no-due' | 'later'

export const FOCUS_RADAR_STATUSES: FocusRadarStatus[] = ['overdue', 'today', 'due-soon', 'waiting', 'no-due', 'later']

const SEVERITY: Record<FocusRadarStatus, number> = {
  overdue: 0,
  today: 1,
  'due-soon': 2,
  waiting: 3,
  'no-due': 4,
  later: 5,
}

/** A deadline within this many days (after today) is "due soon". */
export const DUE_SOON_DAYS = 7

/**
 * Waiting on someone else → its own status (not counted as late); otherwise
 * by deadline: past its effective due time (deadline + time, or the end of the
 * working day) → overdue, today, within `DUE_SOON_DAYS`, later, or none.
 */
export function classifyTodo(item: FocusItem, now: number): FocusRadarStatus {
  const todo = item.todo
  if (todo.status === 'waiting') return 'waiting'
  const left = daysUntil(todo.due_date, now)
  if (left === null) return 'no-due'
  const due = item.effective_due ? Date.parse(item.effective_due) : Number.NaN
  if (left < 0 || (!Number.isNaN(due) && due < now)) return 'overdue'
  if (left === 0) return 'today'
  if (left <= DUE_SOON_DAYS) return 'due-soon'
  return 'later'
}

export interface FocusRadarItem {
  item: FocusItem
  radar: FocusRadarStatus
  /** Calendar days to the deadline (negative = late); null without one. */
  daysLeft: number | null
}

/** Open todos only (done ones are dropped), classified. */
export function classifyTodos(items: FocusItem[], now: number): FocusRadarItem[] {
  return items
    .filter((it) => !it.todo.done && it.bucket !== 'done')
    .map((item) => ({ item, radar: classifyTodo(item, now), daysLeft: daysUntil(item.todo.due_date, now) }))
}

export type FocusRadarCounts = Record<FocusRadarStatus, number>

export function emptyFocusCounts(): FocusRadarCounts {
  return { overdue: 0, today: 0, 'due-soon': 0, waiting: 0, 'no-due': 0, later: 0 }
}

export function countFocus(items: FocusRadarItem[]): FocusRadarCounts {
  const counts = emptyFocusCounts()
  for (const i of items) counts[i.radar]++
  return counts
}

/** Most urgent first, then the latest / nearest deadline, then the backend score. */
export function compareFocus(a: FocusRadarItem, b: FocusRadarItem): number {
  return (
    SEVERITY[a.radar] - SEVERITY[b.radar] ||
    (a.daysLeft ?? Infinity) - (b.daysLeft ?? Infinity) ||
    b.item.score - a.item.score
  )
}

export interface FocusProjectGroup {
  /** `null` = todos without a project (sorted last). */
  projectId: string | null
  counts: FocusRadarCounts
  total: number
  /** Days past the deadline of its latest todo; null when nothing is late. */
  worstLateDays: number | null
  items: FocusRadarItem[]
}

/** One group per project, the ones with the most late work first. */
export function groupByProject(items: FocusRadarItem[]): FocusProjectGroup[] {
  const groups = new Map<string | null, FocusProjectGroup>()
  for (const i of items) {
    const id = i.item.todo.project_id ?? null
    let g = groups.get(id)
    if (!g) {
      g = { projectId: id, counts: emptyFocusCounts(), total: 0, worstLateDays: null, items: [] }
      groups.set(id, g)
    }
    g.items.push(i)
    g.counts[i.radar]++
    g.total++
    if (i.radar === 'overdue') g.worstLateDays = Math.max(g.worstLateDays ?? 0, -(i.daysLeft ?? 0))
  }
  const list = [...groups.values()]
  for (const g of list) g.items.sort(compareFocus)
  return list.sort((a, b) =>
    (a.projectId === null ? 1 : 0) - (b.projectId === null ? 1 : 0) ||
    b.counts.overdue - a.counts.overdue ||
    (b.worstLateDays ?? 0) - (a.worstLateDays ?? 0) ||
    b.counts.today - a.counts.today ||
    b.counts['due-soon'] - a.counts['due-soon'] ||
    b.total - a.total,
  )
}

/** Keep todos of `status` (when set) whose text matches `query`. */
export function filterFocus(
  items: FocusRadarItem[],
  opts: { status?: FocusRadarStatus | null, query?: string },
): FocusRadarItem[] {
  const q = (opts.query ?? '').trim().toLowerCase()
  return items.filter((i) =>
    (!opts.status || i.radar === opts.status) &&
    (!q || i.item.todo.text.toLowerCase().includes(q) || (i.item.todo.source_ref ?? '').toLowerCase().includes(q)),
  )
}
