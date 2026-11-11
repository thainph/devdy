// A todo's deadline (`due_date`, a local calendar date `YYYY-MM-DD`) measured
// against today: how many days are left and how urgent that reads.

/** `overdue` < `today` < `soon` (within SOON_DAYS) < `later`. */
export type DueStatus = 'overdue' | 'today' | 'soon' | 'later'

/** "Soon" covers tomorrow up to this many days ahead. */
export const SOON_DAYS = 3

/** Today as a local `YYYY-MM-DD`. */
export function localYmd(d: Date = new Date()): string {
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
}

function ymdDayNumber(ymd: string): number | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(ymd)
  if (!m) return null
  return Date.UTC(+m[1], +m[2] - 1, +m[3]) / 86_400_000
}

/** Whole days from `today` to `due` (negative = past); null for a missing / malformed date. */
export function daysUntil(due: string | null | undefined, today: string): number | null {
  if (!due) return null
  const a = ymdDayNumber(today)
  const b = ymdDayNumber(due)
  return a == null || b == null ? null : Math.round(b - a)
}

export function dueStatus(due: string | null | undefined, today: string): DueStatus | null {
  const days = daysUntil(due, today)
  if (days == null) return null
  if (days < 0) return 'overdue'
  if (days === 0) return 'today'
  if (days <= SOON_DAYS) return 'soon'
  return 'later'
}

/** What `compareByDue` reads off a todo (an `ItemRow` fits). */
export interface DueSortable {
  dueDate: string | null
  done: boolean
  /** ISO creation time: the tie-breaker, oldest first. */
  at: string
}

/**
 * "What do I do first": open before done, then the earliest deadline (overdue
 * on top), todos without one last; equal deadlines keep the oldest first.
 */
export function compareByDue(a: DueSortable, b: DueSortable): number {
  if (a.done !== b.done) return a.done ? 1 : -1
  const da = a.dueDate && ymdDayNumber(a.dueDate) != null ? a.dueDate : null
  const db = b.dueDate && ymdDayNumber(b.dueDate) != null ? b.dueDate : null
  if (da !== db) {
    if (!da) return 1
    if (!db) return -1
    return da < db ? -1 : 1
  }
  return a.at < b.at ? -1 : a.at > b.at ? 1 : 0
}
