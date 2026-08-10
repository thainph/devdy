// Small date helpers for the unified Google Calendar screen. Uses the native
// Date API only (no external date lib), mirroring the local-time formatting
// convention used elsewhere (see WorkDigestView `fmtLocal`).

/** Local-time YYYY-MM-DD (NOT toISOString, which shifts to UTC). */
export function fmtLocal(d: Date): string {
  const y = d.getFullYear()
  const m = String(d.getMonth() + 1).padStart(2, '0')
  const day = String(d.getDate()).padStart(2, '0')
  return `${y}-${m}-${day}`
}

export function addDays(d: Date, n: number): Date {
  const x = new Date(d)
  x.setDate(x.getDate() + n)
  return x
}

export function startOfDay(d: Date): Date {
  const x = new Date(d)
  x.setHours(0, 0, 0, 0)
  return x
}

/** Monday-based start of the week containing `d`. */
export function startOfWeek(d: Date): Date {
  const x = startOfDay(d)
  const dow = (x.getDay() + 6) % 7 // 0 = Monday
  return addDays(x, -dow)
}

export function startOfMonth(d: Date): Date {
  return new Date(d.getFullYear(), d.getMonth(), 1)
}

export function endOfMonth(d: Date): Date {
  return new Date(d.getFullYear(), d.getMonth() + 1, 0)
}

export function sameDay(a: Date, b: Date): boolean {
  return (
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate()
  )
}

/** Minutes since midnight (local time). */
export function minutesOfDay(d: Date): number {
  return d.getHours() * 60 + d.getMinutes()
}

export interface ParsedEventTime {
  start: Date
  end: Date
  allDay: boolean
}

/**
 * Parse a CalEvent's start/end into local Date objects. Timed events carry an
 * RFC3339 dateTime; all-day events carry a plain YYYY-MM-DD (Google's end date
 * is EXCLUSIVE, so we pull it back by a day for display).
 */
export function parseEventTime(ev: { start: string; end: string; all_day: boolean }): ParsedEventTime {
  if (ev.all_day) {
    const start = new Date(`${ev.start}T00:00:00`)
    let end = ev.end ? new Date(`${ev.end}T00:00:00`) : new Date(start)
    // Exclusive end → step back to the last covered day; guard single-day events.
    end = addDays(end, -1)
    if (end < start) end = new Date(start)
    return { start, end, allDay: true }
  }
  const start = new Date(ev.start)
  const end = ev.end ? new Date(ev.end) : new Date(start.getTime() + 30 * 60_000)
  return { start, end, allDay: false }
}

/** Fixed palette assigned per account (index-based) so colours stay stable. */
export const ACCOUNT_COLORS = [
  '#6366f1', // indigo
  '#22c55e', // green
  '#f59e0b', // amber
  '#ec4899', // pink
  '#06b6d4', // cyan
  '#a855f7', // purple
  '#ef4444', // red
  '#14b8a6', // teal
]

export function colorForIndex(i: number): string {
  return ACCOUNT_COLORS[i % ACCOUNT_COLORS.length]
}
