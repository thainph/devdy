// The session screen's History rail view: what the search box's filter menu
// sets (a status filter and the row density) — its defaults, which runs pass
// it, the per-status counts the menu shows, and the "Filtering: …" summary.
// Pure, so it is testable apart from RunView.

export type RunHistoryDensity = 'comfortable' | 'compact' | 'dense'

/**
 * '' = every session. 'waiting' = waiting for a permission / question answer,
 * 'unread' = finished with output not looked at yet, 'running' = a live turn.
 */
export type RunHistoryStatus = '' | 'waiting' | 'unread' | 'running'

export const RUN_HISTORY_STATUSES: RunHistoryStatus[] = ['', 'waiting', 'unread', 'running']
export const RUN_HISTORY_DENSITIES: RunHistoryDensity[] = ['comfortable', 'compact', 'dense']

export interface RunHistoryView {
  status: RunHistoryStatus
  /** Persisted app-wide (uiLayout.listDensity); the rail only shows / sets it. */
  density: RunHistoryDensity
}

export function defaultRunHistoryView(): RunHistoryView {
  return { status: '', density: 'compact' }
}

/** What the status filter looks at for one run. */
export interface RunHistoryFlags {
  waiting: boolean
  unread: boolean
  running: boolean
}

export function matchesRunHistoryStatus(status: RunHistoryStatus, flags: RunHistoryFlags): boolean {
  return !status || flags[status]
}

/** How many runs each status would show ('' = all of them). */
export function runHistoryStatusCounts(flags: RunHistoryFlags[]): Record<RunHistoryStatus, number> {
  const counts: Record<RunHistoryStatus, number> = { '': flags.length, waiting: 0, unread: 0, running: 0 }
  for (const f of flags) {
    if (f.waiting) counts.waiting++
    if (f.unread) counts.unread++
    if (f.running) counts.running++
  }
  return counts
}

/** The status narrows the list (what the summary line and "Clear filters" are about). */
export function isRunHistoryFiltering(view: RunHistoryView): boolean {
  return !!view.status
}

/** Anything differs from the defaults: the filter button shows its accent dot. */
export function runHistoryViewChanged(view: RunHistoryView): boolean {
  return isRunHistoryFiltering(view) || view.density !== defaultRunHistoryView().density
}

export function runHistoryFilterSummary(
  view: RunHistoryView,
  label: (s: Exclude<RunHistoryStatus, ''>) => string,
): string[] {
  return view.status ? [label(view.status)] : []
}

/** Free-text search: the trimmed query appears (case-insensitive) in the run's shown text. */
export function matchesRunSearch(query: string, haystack: (string | null | undefined)[]): boolean {
  const q = query.trim().toLowerCase()
  if (!q) return true
  return haystack.filter(Boolean).join(' ').toLowerCase().includes(q)
}
