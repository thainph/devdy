// Pure filtering / search / bot-list / badge helpers for the PR inbox
// (PrInboxView). Sibling of prInboxGrouping.ts — same rules: no Vue, no i18n,
// unit-tested with `node --test src/lib/__tests__/prInboxFilters.test.mjs`.
import type { PrInboxItem } from '@/stores/prInbox'

/**
 * Review state of a PR from the inbox's point of view:
 * - `unmapped`:     repo isn't tracked in devdy (no Review action)
 * - `not_reviewed`: mapped, no review run yet
 * - `running`:      the linked review run is streaming
 * - `reviewed`:     the linked review run exists and isn't running
 */
export type PrReviewStatus = 'not_reviewed' | 'running' | 'reviewed' | 'unmapped'
export const PR_REVIEW_STATUSES: readonly PrReviewStatus[] = ['not_reviewed', 'running', 'reviewed', 'unmapped']

/** Filter value standing for "Not in devdy" in the project filter. */
export const UNMAPPED_PROJECT_VALUE = '__unmapped__'

/** Multi-select filters; an empty list means "no filter" for that dimension. */
export interface PrFilters {
  /** project_id, or UNMAPPED_PROJECT_VALUE */
  projects: string[]
  /** normalized author logins */
  authors: string[]
  /** reviewer account ids */
  accounts: string[]
  statuses: PrReviewStatus[]
}

export function emptyFilters(): PrFilters {
  return { projects: [], authors: [], accounts: [], statuses: [] }
}

export function hasActiveFilters(f: PrFilters): boolean {
  return f.projects.length + f.authors.length + f.accounts.length + f.statuses.length > 0
}

/** Effective review status, given the linked run's current status (live
 * stream status if any, else the DB snapshot). */
export function reviewStatusOf(item: PrInboxItem, runStatus: string | null): PrReviewStatus {
  if (!item.mapped) return 'unmapped'
  if (!item.existing_run_id) return 'not_reviewed'
  return runStatus === 'running' || runStatus === 'fetched' ? 'running' : 'reviewed'
}

// ---------------------------------------------------------------------------
// Persistence validation
// ---------------------------------------------------------------------------

function stringList(raw: unknown, normalize: (s: string) => string = s => s): string[] {
  if (!Array.isArray(raw)) return []
  const out: string[] = []
  for (const v of raw) {
    if (typeof v !== 'string') continue
    const n = normalize(v)
    if (n && !out.includes(n)) out.push(n)
  }
  return out
}

/** Login comparison form: trimmed + lowercased (GitHub logins are
 * case-insensitive). */
export function normalizeLogin(login: string): string {
  return login.trim().toLowerCase()
}

/** Validate filters read back from storage — unknown shapes and values become
 * "no filter" instead of throwing. */
export function sanitizeFilters(raw: unknown): PrFilters {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return emptyFilters()
  const r = raw as Record<string, unknown>
  return {
    projects: stringList(r.projects),
    authors: stringList(r.authors, normalizeLogin),
    accounts: stringList(r.accounts),
    statuses: stringList(r.statuses).filter((s): s is PrReviewStatus =>
      (PR_REVIEW_STATUSES as readonly string[]).includes(s),
    ),
  }
}

/** Validate the custom bot-login list (normalized, unique, non-empty). */
export function sanitizeBotLogins(raw: unknown): string[] {
  return stringList(raw, normalizeLogin)
}

// ---------------------------------------------------------------------------
// Bots
// ---------------------------------------------------------------------------

/** Bot classification: the backend's `author_is_bot` OR a login on the user's
 * custom bot list. */
export function makeBotPredicate(customBotLogins: readonly string[]): (item: PrInboxItem) => boolean {
  const custom = new Set(customBotLogins.map(normalizeLogin))
  return item => item.author_is_bot || custom.has(normalizeLogin(item.author_login))
}

// ---------------------------------------------------------------------------
// Filter options
// ---------------------------------------------------------------------------

export interface PrFilterOption {
  value: string
  /** Display label; empty for UNMAPPED_PROJECT_VALUE (the view translates it). */
  label: string
  count: number
  avatarUrl?: string | null
}

export interface PrFilterOptions {
  projects: PrFilterOption[]
  authors: PrFilterOption[]
  accounts: PrFilterOption[]
  statuses: PrFilterOption[]
}

/** The reviewer accounts of a PR (falls back to the first-hit account for
 * items from an older backend). */
export function reviewerAccountsOf(item: PrInboxItem): { account_id: string; account_label: string }[] {
  return item.reviewer_accounts?.length
    ? item.reviewer_accounts
    : [{ account_id: item.account_id, account_label: item.account_label }]
}

function tally(map: Map<string, PrFilterOption>, value: string, label: string, avatarUrl?: string | null) {
  const existing = map.get(value)
  if (existing) {
    existing.count += 1
    if (!existing.avatarUrl && avatarUrl) existing.avatarUrl = avatarUrl
  } else {
    map.set(value, { value, label, count: 1, ...(avatarUrl !== undefined ? { avatarUrl } : {}) })
  }
}

function sortedOptions(map: Map<string, PrFilterOption>): PrFilterOption[] {
  return [...map.values()].sort((a, b) => b.count - a.count || a.label.localeCompare(b.label))
}

/** Options (with PR counts) for each filter, derived from the given items.
 * Unmapped sorts last among projects; statuses keep their canonical order. */
export function buildFilterOptions(
  items: readonly PrInboxItem[],
  statusOf: (item: PrInboxItem) => PrReviewStatus,
): PrFilterOptions {
  const projects = new Map<string, PrFilterOption>()
  const authors = new Map<string, PrFilterOption>()
  const accounts = new Map<string, PrFilterOption>()
  const statusCounts = new Map<PrReviewStatus, number>()
  for (const item of items) {
    if (item.mapped && item.project_id) {
      tally(projects, item.project_id, item.project_name?.trim() || item.project_id)
    } else {
      tally(projects, UNMAPPED_PROJECT_VALUE, '')
    }
    const login = item.author_login.trim()
    tally(authors, normalizeLogin(login), login, item.author_avatar_url ?? null)
    for (const acc of reviewerAccountsOf(item)) {
      tally(accounts, acc.account_id, acc.account_label.trim() || acc.account_id)
    }
    const status = statusOf(item)
    statusCounts.set(status, (statusCounts.get(status) ?? 0) + 1)
  }
  const unmapped = projects.get(UNMAPPED_PROJECT_VALUE)
  projects.delete(UNMAPPED_PROJECT_VALUE)
  return {
    projects: [...sortedOptions(projects), ...(unmapped ? [unmapped] : [])],
    authors: sortedOptions(authors),
    accounts: sortedOptions(accounts),
    statuses: PR_REVIEW_STATUSES.filter(s => statusCounts.has(s)).map(s => ({
      value: s,
      label: '',
      count: statusCounts.get(s)!,
    })),
  }
}

/**
 * Drop stored filter values that no longer match any current PR (a project
 * whose PRs were merged, a removed account…). Applied on read only, so the
 * saved choice comes back if the value reappears — and a stale value can never
 * filter the inbox down to nothing.
 */
export function effectiveFilters(filters: PrFilters, options: PrFilterOptions): PrFilters {
  const keep = <T extends string>(values: T[], opts: PrFilterOption[]) => {
    const known = new Set(opts.map(o => o.value))
    return values.filter(v => known.has(v))
  }
  return {
    projects: keep(filters.projects, options.projects),
    authors: keep(filters.authors, options.authors),
    accounts: keep(filters.accounts, options.accounts),
    statuses: keep(filters.statuses, options.statuses),
  }
}

// ---------------------------------------------------------------------------
// Search + filtering
// ---------------------------------------------------------------------------

/**
 * Free-text search: case-insensitive substring of the title, `owner/repo` or
 * author login; `123` / `#123` also match the PR number exactly.
 */
export function matchesSearch(item: PrInboxItem, query: string): boolean {
  const q = query.trim().toLowerCase()
  if (!q) return true
  const num = /^#?(\d+)$/.exec(q)
  if (num && item.number === Number(num[1])) return true
  return (
    item.title.toLowerCase().includes(q) ||
    `${item.owner}/${item.repo}`.toLowerCase().includes(q) ||
    item.author_login.toLowerCase().includes(q)
  )
}

/** Apply (already effective) filters + search. Dimensions AND together; values
 * within a dimension OR together. */
export function applyFilters(
  items: readonly PrInboxItem[],
  filters: PrFilters,
  query: string,
  statusOf: (item: PrInboxItem) => PrReviewStatus,
): PrInboxItem[] {
  const projects = new Set(filters.projects)
  const authors = new Set(filters.authors)
  const accounts = new Set(filters.accounts)
  const statuses = new Set<string>(filters.statuses)
  return items.filter(item => {
    if (projects.size > 0) {
      const p = item.mapped && item.project_id ? item.project_id : UNMAPPED_PROJECT_VALUE
      if (!projects.has(p)) return false
    }
    if (authors.size > 0 && !authors.has(normalizeLogin(item.author_login))) return false
    if (accounts.size > 0 && !reviewerAccountsOf(item).some(a => accounts.has(a.account_id))) return false
    if (statuses.size > 0 && !statuses.has(statusOf(item))) return false
    return matchesSearch(item, query)
  })
}

// ---------------------------------------------------------------------------
// Card badges
// ---------------------------------------------------------------------------

export const STALE_AFTER_DAYS = 7

/** True when the PR hasn't been updated for more than `days` days. */
export function isStale(updatedAt: string, now: number, days: number = STALE_AFTER_DAYS): boolean {
  const t = Date.parse(updatedAt)
  return Number.isFinite(t) && now - t > days * 86_400_000
}

/** First `max` labels plus how many are left over (shown as "+N"). */
export function splitLabels(labels: readonly string[], max = 3): { shown: string[]; extra: number } {
  return { shown: labels.slice(0, max), extra: Math.max(0, labels.length - max) }
}

// ---------------------------------------------------------------------------
// Bulk review
// ---------------------------------------------------------------------------

export const BULK_REVIEW_CAP = 10

/**
 * Whether a review run may still be started for a PR, judged on its CURRENT
 * store entry (undefined = gone after a refresh): mapped (project + repo
 * known), no linked run, not already being started. The bulk loop re-checks
 * this before every item, so a refresh during the confirm dialog or mid-batch
 * can't make it start a duplicate run from a stale snapshot.
 */
export function canStartReview(item: PrInboxItem | undefined, reviewing: boolean): item is PrInboxItem {
  if (!item || reviewing) return false
  return item.mapped && !!item.project_id && !!item.repo_id && !item.existing_run_id
}

/**
 * PRs of a group that "AI review all" would start: mapped (project + repo
 * known), no linked run yet, not already being started. Deduped by PR (a group
 * never repeats one, but stay safe) and capped at `cap`; `total` is the
 * uncapped count so the confirm dialog can say how many were left out.
 */
export function bulkReviewCandidates(
  items: readonly PrInboxItem[],
  isReviewing: (item: PrInboxItem) => boolean,
  cap: number = BULK_REVIEW_CAP,
): { items: PrInboxItem[]; total: number } {
  const seen = new Set<string>()
  const eligible = items.filter(item => {
    if (!canStartReview(item, isReviewing(item))) return false
    const key = `${item.owner}/${item.repo}#${item.number}`.toLowerCase()
    if (seen.has(key)) return false
    seen.add(key)
    return true
  })
  return { items: eligible.slice(0, cap), total: eligible.length }
}
