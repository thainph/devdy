// Work Dashboard — "PRs waiting for my review": which review to do first.
//
// Pure logic (no Vue / Tauri) for node:test, over the PR Inbox items (GitHub
// PRs where the user was asked to review directly). Order:
//  1. PRs with new commits after the AI review (re-review), then the longest
//     waiting (> 1 working day amber, > 3 red);
//  2. bonus for green CI, a small PR (< 200 changed lines) and being the last
//     pending reviewer;
//  3. drafts after everything else; bot PRs folded into one group. Failing CI
//     is labelled, never dropped.
// The wait is counted from the review request when GitHub reported it;
// without it there is no wait — the UI shows `updated_at` as "updated" instead.

/** The `PrInboxItem` fields read here (snake_case, as the backend sends them). */
export interface PrLite {
  owner: string
  repo: string
  number: number
  draft: boolean
  author_login: string
  author_is_bot: boolean
  updated_at: string
  existing_run_id: string | null
  existing_run_status: string | null
  updated_since_review: boolean
  requested_reviewers: unknown[]
  additions?: number | null
  deletions?: number | null
  ci_state?: string | null
  review_requested_at?: string | null
}

export type PrWaitLevel = 'ok' | 'warn' | 'late'
export type AiReviewBadge = 'none' | 'running' | 'done' | 'updated'
export type CiStatus = 'success' | 'failure' | 'pending' | null

/** Changed lines (additions + deletions) under this make a "small" PR. */
export const SMALL_PR_LINES = 200
export const WAIT_WARN_WORKDAYS = 1
export const WAIT_LATE_WORKDAYS = 3

const DAY = 86_400_000
const RUNNING = new Set(['running', 'pending', 'queued'])

/**
 * Working time between two instants, in days: weekend hours (Sat/Sun, local
 * time) are not counted. 0 when `to` ≤ `from`.
 */
export function workdaysBetween(from: number, to: number): number {
  if (!(to > from)) return 0
  let ms = 0
  let t = from
  while (t < to) {
    const d = new Date(t)
    const next = new Date(d.getFullYear(), d.getMonth(), d.getDate() + 1).getTime()
    const end = Math.min(next, to)
    const dow = d.getDay()
    if (dow !== 0 && dow !== 6) ms += end - t
    t = end
  }
  return ms / DAY
}

export function prWaitLevel(workdays: number | null): PrWaitLevel {
  if (workdays === null) return 'ok'
  if (workdays > WAIT_LATE_WORKDAYS) return 'late'
  if (workdays > WAIT_WARN_WORKDAYS) return 'warn'
  return 'ok'
}

export function ciStatus(state: string | null | undefined): CiStatus {
  switch ((state ?? '').toUpperCase()) {
    case 'SUCCESS': return 'success'
    case 'FAILURE':
    case 'ERROR': return 'failure'
    case 'PENDING':
    case 'EXPECTED': return 'pending'
    default: return null
  }
}

export function aiReviewBadge(pr: PrLite): AiReviewBadge {
  if (!pr.existing_run_id) return 'none'
  if (RUNNING.has(pr.existing_run_status ?? '')) return 'running'
  if (pr.updated_since_review) return 'updated'
  return 'done'
}

export function changedLines(pr: PrLite): number | null {
  if (pr.additions == null && pr.deletions == null) return null
  return (pr.additions ?? 0) + (pr.deletions ?? 0)
}

export interface RankedPr<T extends PrLite = PrLite> {
  pr: T
  /** Working days since the review request; null when unknown. */
  waitDays: number | null
  waitLevel: PrWaitLevel
  ci: CiStatus
  ai: AiReviewBadge
  small: boolean
  /** No other reviewer is still pending. */
  soleReviewer: boolean
  score: number
}

export function rankPr<T extends PrLite>(pr: T, now: number): RankedPr<T> {
  const requested = pr.review_requested_at ? Date.parse(pr.review_requested_at) : Number.NaN
  const waitDays = Number.isNaN(requested) ? null : workdaysBetween(requested, now)
  const level = prWaitLevel(waitDays)
  const ci = ciStatus(pr.ci_state)
  const lines = changedLines(pr)
  const small = lines !== null && lines < SMALL_PR_LINES
  const soleReviewer = pr.requested_reviewers.length === 0
  const ai = aiReviewBadge(pr)
  let score = 0
  if (ai === 'updated') score += 100
  if (level === 'late') score += 60
  else if (level === 'warn') score += 30
  if (ci === 'success') score += 10
  if (small) score += 10
  if (soleReviewer) score += 10
  return { pr, waitDays, waitLevel: level, ci, ai, small, soleReviewer, score }
}

/** Highest score, then the longest wait, then the most recently updated. */
export function compareRanked(a: RankedPr, b: RankedPr): number {
  return (
    Number(a.pr.draft) - Number(b.pr.draft) ||
    b.score - a.score ||
    (b.waitDays ?? -1) - (a.waitDays ?? -1) ||
    b.pr.updated_at.localeCompare(a.pr.updated_at) ||
    a.pr.number - b.pr.number
  )
}

export interface PrPriorityResult<T extends PrLite = PrLite> {
  /** Human PRs, drafts last. */
  rows: RankedPr<T>[]
  /** Bot PRs (one folded row in the UI). */
  bots: RankedPr<T>[]
}

export function rankPrs<T extends PrLite>(
  items: T[],
  now: number,
  isBot: (pr: T) => boolean = (pr) => pr.author_is_bot,
): PrPriorityResult<T> {
  const rows: RankedPr<T>[] = []
  const bots: RankedPr<T>[] = []
  for (const pr of items) (isBot(pr) ? bots : rows).push(rankPr(pr, now))
  rows.sort(compareRanked)
  bots.sort(compareRanked)
  return { rows, bots }
}

/** PRs counted as "waiting for my review" (drafts and bots aside). */
export function actionableCount(r: PrPriorityResult): number {
  return r.rows.filter((x) => !x.pr.draft).length
}
