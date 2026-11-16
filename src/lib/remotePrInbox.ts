// Pure logic behind the Remote Controller's PR tab (the desktop PR Inbox on a
// phone): which review state a PR is in, which action its button offers, and
// the search / bot / project filter. Tauri- and i18n-free so node:test can run it.

/** The fields of a PR these helpers read (a subset of the wire `PrInboxInfo`). */
export interface PrLike {
  key: string
  owner: string
  repo: string
  number: number
  title: string
  author_login: string
  author_is_bot: boolean
  labels: string[]
  updated_at: string
  mapped: boolean
  project_id: string | null
  existing_run_id: string | null
  existing_run_status: string | null
  updated_since_review: boolean
}

/** A run as the phone's run list knows it (live status). */
export interface RunStatusLike {
  id: string
  status: string
}

/** Where a PR's review stands (desktop `reviewStatusOf`). */
export type PrReviewState = 'unmapped' | 'not_reviewed' | 'running' | 'reviewed' | 'updated'

/** What the PR's main button does. */
export type PrAction = 'review' | 'starting' | 'view' | 'review_again' | 'none'

/**
 * The review run's status: the live run list wins (it follows every turn),
 * the inbox's snapshot is the fallback for a run the list does not hold.
 */
export function reviewRunStatus(pr: PrLike, runs: readonly RunStatusLike[]): string | null {
  if (!pr.existing_run_id) return null
  return runs.find((r) => r.id === pr.existing_run_id)?.status ?? pr.existing_run_status
}

function isActive(status: string | null): boolean {
  return status === 'running' || status === 'fetched'
}

export function prReviewState(pr: PrLike, runStatus: string | null): PrReviewState {
  if (!pr.mapped) return 'unmapped'
  if (!pr.existing_run_id) return 'not_reviewed'
  if (isActive(runStatus)) return 'running'
  return pr.updated_since_review ? 'updated' : 'reviewed'
}

/**
 * The main button: Review a PR with no session, open a running one, offer
 * Review again on a finished one (highlighted when new commits landed — the
 * card itself still opens the session). `busy` = a start / re-review this
 * phone sent is in flight.
 */
export function prAction(state: PrReviewState, busy: boolean): PrAction {
  if (busy) return 'starting'
  switch (state) {
    case 'unmapped':
      return 'none'
    case 'not_reviewed':
      return 'review'
    case 'running':
      return 'view'
    case 'reviewed':
    case 'updated':
      return 'review_again'
  }
}

export interface PrFilter {
  query: string
  hideBots: boolean
  /** null = every project (unmapped PRs included). */
  projectId: string | null
}

/** Whether `pr` matches the search: number, repo, title, author or a label. */
export function matchesPrQuery(pr: PrLike, query: string): boolean {
  const q = query.trim().toLowerCase().replace(/^#/, '')
  if (!q) return true
  const hay = [
    String(pr.number),
    `${pr.owner}/${pr.repo}`,
    `${pr.repo}#${pr.number}`,
    pr.title,
    pr.author_login,
    ...pr.labels,
  ]
    .join('\n')
    .toLowerCase()
  return q.split(/\s+/).every((word) => hay.includes(word))
}

/**
 * The PRs to show, newest activity first. A project scope keeps that
 * project's PRs only (an unmapped PR belongs to none).
 */
export function filterPrs<T extends PrLike>(items: readonly T[], filter: PrFilter): T[] {
  return items
    .filter((pr) => !(filter.hideBots && pr.author_is_bot))
    .filter((pr) => filter.projectId === null || pr.project_id === filter.projectId)
    .filter((pr) => matchesPrQuery(pr, filter.query))
    .sort((a, b) => Date.parse(b.updated_at || '0') - Date.parse(a.updated_at || '0') || a.key.localeCompare(b.key))
}

/** How many bot PRs the bot switch hides from `items`. */
export function botCount(items: readonly PrLike[]): number {
  return items.filter((pr) => pr.author_is_bot).length
}
