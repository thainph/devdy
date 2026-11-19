// Dashboard History — every session that ran, in every project, latest
// activity first, paged as the list scrolls (`list_run_history`). The Rust side
// mirrors `HistoryRun` with `#[serde(rename_all = "camelCase")]`.
//
// Pure types + helpers (no Vue / Tauri) so node:test can import it.

/** Tauri command returning one page of {@link HistoryRun}. */
export const CMD_RUN_HISTORY = 'list_run_history'

/** Rows asked for per page. */
export const HISTORY_PAGE = 40
/** Most rows one request may return (the backend caps it too). */
export const HISTORY_MAX_PAGE = 200

export interface HistoryRun {
  runId: string
  projectId: string
  projectName: string
  /** `session` | `analyze_issue` | `review_pr` | … */
  runType: string
  refNumber: number | null
  title: string | null
  /** Issue / PR title, when the run is about one. */
  refTitle: string | null
  status: string
  engine: string
  role: string | null
  /** Workers this run spawned (a conductor), 0 otherwise. */
  workerCount: number
  pinned: boolean
  /** Finished after the user last saw it (`runs.seen_at`). */
  unread: boolean
  /** As stored; the sort key, with `runId` as tie-break. */
  lastActivityAt: string
}

/** "PR" / "Issue" for a run about one, else null. */
export function historyKind(r: Pick<HistoryRun, 'runType'>): 'PR' | 'Issue' | null {
  if (r.runType === 'session') return null
  return r.runType === 'analyze_issue' ? 'Issue' : 'PR'
}

/** The name to show: its title, else the issue / PR it is about, else null. */
export function historyTitle(r: Pick<HistoryRun, 'title' | 'refTitle' | 'runType' | 'refNumber'>): string | null {
  if (r.title?.trim()) return r.title.trim()
  const kind = historyKind(r)
  if (kind && r.refNumber != null) return r.refTitle ? `#${r.refNumber} ${r.refTitle}` : `${kind} #${r.refNumber}`
  return r.refTitle?.trim() || null
}

function pad(n: number): string {
  return String(n).padStart(2, '0')
}

/** "HH:mm" for today, "dd/MM HH:mm" for another day; empty when unreadable. */
export function historyWhen(iso: string, now: number): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return ''
  const time = `${pad(d.getHours())}:${pad(d.getMinutes())}`
  if (d.toDateString() === new Date(now).toDateString()) return time
  return `${pad(d.getDate())}/${pad(d.getMonth() + 1)} ${time}`
}

/**
 * Put a freshly fetched head (the newest rows) in front of the rows already
 * loaded: the head wins for any run in both, and loaded rows older than the
 * head's last one are kept after it (they were further down the scroll).
 * With `replace`, the head is the whole list.
 */
export function mergeHistory(loaded: readonly HistoryRun[], head: readonly HistoryRun[], replace = false): HistoryRun[] {
  if (replace || !head.length) return replace ? [...head] : [...loaded]
  const ids = new Set(head.map((r) => r.runId))
  const last = head[head.length - 1]
  const older = loaded.filter((r) => !ids.has(r.runId) && isOlder(r, last))
  return [...head, ...older]
}

/** `a` sorts after `b` (older activity, then smaller id) — the backend's order. */
export function isOlder(a: Pick<HistoryRun, 'lastActivityAt' | 'runId'>, b: Pick<HistoryRun, 'lastActivityAt' | 'runId'>): boolean {
  return a.lastActivityAt < b.lastActivityAt || (a.lastActivityAt === b.lastActivityAt && a.runId < b.runId)
}

/** Append an older page, skipping runs already listed. */
export function appendHistory(loaded: readonly HistoryRun[], page: readonly HistoryRun[]): HistoryRun[] {
  const ids = new Set(loaded.map((r) => r.runId))
  return [...loaded, ...page.filter((r) => !ids.has(r.runId))]
}
