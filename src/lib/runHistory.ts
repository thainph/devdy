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
  /** Skipped by "Clear all". */
  protected: boolean
  /** Finished after the user last saw it (`runs.seen_at`). */
  unread: boolean
  /** Label of the Claude account it runs on (Claude runs only). */
  accountName: string | null
  /** `github` | `gitlab` for an issue / PR run with a repo. */
  repoProvider: string | null
  /** `owner/repo` (GitHub) or the project path (GitLab). */
  repoPath: string | null
  /** As stored; the sort key, with `runId` as tie-break. */
  lastActivityAt: string
}

/** "PR" / "Issue" for a run about one, else null. */
export function historyKind(r: Pick<HistoryRun, 'runType'>): 'PR' | 'Issue' | null {
  if (r.runType === 'session') return null
  return r.runType === 'analyze_issue' ? 'Issue' : 'PR'
}

/**
 * What the row says about its kind: an issue / PR, a conductor, or a plain
 * session. Every session can conduct, so only one that actually spawned
 * workers reads as a conductor.
 */
export function historyType(r: Pick<HistoryRun, 'runType' | 'workerCount'>): 'pr' | 'issue' | 'conductor' | 'session' {
  const kind = historyKind(r)
  if (kind) return kind === 'PR' ? 'pr' : 'issue'
  return r.workerCount > 0 ? 'conductor' : 'session'
}

/** `owner/repo#12` for an issue / PR run whose repo is known, else null. */
export function historyRepoRef(r: Pick<HistoryRun, 'runType' | 'repoPath' | 'refNumber'>): string | null {
  if (!historyKind(r) || !r.repoPath) return null
  return r.refNumber != null ? `${r.repoPath}#${r.refNumber}` : r.repoPath
}

/**
 * The issue / PR page on GitHub or GitLab, else null. Self-hosted GitLab hosts
 * are not stored, so GitLab links assume gitlab.com (as `repoWebUrl` does).
 */
export function historyWebUrl(r: Pick<HistoryRun, 'runType' | 'repoPath' | 'repoProvider' | 'refNumber'>): string | null {
  const kind = historyKind(r)
  if (!kind || !r.repoPath || r.refNumber == null) return null
  if (r.repoProvider === 'gitlab') {
    return `https://gitlab.com/${r.repoPath}/-/${kind === 'PR' ? 'merge_requests' : 'issues'}/${r.refNumber}`
  }
  return `https://github.com/${r.repoPath}/${kind === 'PR' ? 'pull' : 'issues'}/${r.refNumber}`
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

/** Days of "N days ago" before the row switches to a date. */
export const HISTORY_AGO_DAYS = 7

/**
 * How long ago, as an i18n key + count: under a minute → `now`; today →
 * `minutes` / `hours`; the day before → `yesterday`; within
 * {@link HISTORY_AGO_DAYS} calendar days → `days`; else `date` ("dd/MM").
 * Null when unreadable. A time slightly in the future (clock skew) is `now`.
 */
export function historyAgo(iso: string, now: number): { key: 'now' | 'minutes' | 'hours' | 'yesterday' | 'days' | 'date'; n: number; date: string } | null {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return null
  const date = `${pad(d.getDate())}/${pad(d.getMonth() + 1)}`
  const diff = now - d.getTime()
  if (diff < 60_000) return { key: 'now', n: 0, date }
  const today = new Date(now)
  if (d.toDateString() === today.toDateString()) {
    const minutes = Math.floor(diff / 60_000)
    return minutes < 60 ? { key: 'minutes', n: minutes, date } : { key: 'hours', n: Math.floor(minutes / 60), date }
  }
  const dayStart = (t: Date) => new Date(t.getFullYear(), t.getMonth(), t.getDate()).getTime()
  const days = Math.round((dayStart(today) - dayStart(d)) / 86_400_000)
  if (days === 1) return { key: 'yesterday', n: 1, date }
  if (days > 1 && days <= HISTORY_AGO_DAYS) return { key: 'days', n: days, date }
  return { key: 'date', n: 0, date }
}

/** The exact time, for the tooltip: "dd/MM/yyyy HH:mm". */
export function historyExact(iso: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return ''
  return `${pad(d.getDate())}/${pad(d.getMonth() + 1)}/${d.getFullYear()} ${pad(d.getHours())}:${pad(d.getMinutes())}`
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
