// GitHub Mentions: types mirroring `src-tauri/src/github_mentions`, plus the
// pure filtering / grouping the view applies (tested in __tests__).

export type MentionReason = 'mention' | 'team_mention' | 'comment' | 'author'
export const MENTION_REASONS: MentionReason[] = ['mention', 'team_mention', 'comment', 'author']

export interface MentionComment {
  /** 0 when the trigger is the issue / PR description. */
  id: number
  author_login: string
  author_avatar_url: string
  body: string
  html_url: string
  created_at: string
  review_comment: boolean
  path: string | null
}

export interface MentionItem {
  key: string
  thread_id: string
  account_id: string
  account_label: string
  account_login: string
  reason: MentionReason | string
  unread: boolean
  kind: 'PullRequest' | 'Issue' | 'Discussion' | string
  owner: string
  repo: string
  number: number | null
  title: string
  html_url: string
  updated_at: string
  trigger: MentionComment | null
  newer_comments: number
  answered: boolean
  answered_at: string | null
  /** The Devdy project / repo tracking this GitHub repo, if any. */
  project_id: string | null
  repo_id: string | null
  project_name: string | null
  /** The "Check with AI" session started for this mention. */
  existing_run_id: string | null
  existing_run_status: string | null
}

export interface MentionAccountError {
  account_id: string
  account_label: string
  message: string
}

export interface MentionsResult {
  items: MentionItem[]
  errors: MentionAccountError[]
}

export interface CachedMentions extends MentionsResult {
  fetched_at: string
}

export interface MentionFilters {
  reasons: string[]
  accounts: string[]
  repos: string[]
}

export type MentionFilterDimension = keyof MentionFilters

export const EMPTY_MENTION_FILTERS: MentionFilters = { reasons: [], accounts: [], repos: [] }

export type MentionGroupBy = 'repo' | 'person' | 'none'
export const MENTION_GROUP_BYS: MentionGroupBy[] = ['repo', 'person', 'none']

/** Waiting this long counts as stale (orange time), like the PR Inbox. */
export const STALE_AFTER_DAYS = 3

export function repoOf(item: MentionItem): string {
  return `${item.owner}/${item.repo}`
}

/** When the mention happened (the trigger comment, else the thread activity). */
export function mentionedAt(item: MentionItem): string {
  return item.trigger?.created_at ?? item.updated_at
}

export function isStale(item: MentionItem, now = Date.now()): boolean {
  const at = Date.parse(mentionedAt(item))
  return !item.answered && Number.isFinite(at) && now - at > STALE_AFTER_DAYS * 86_400_000
}

export function sanitizeFilters(raw: unknown): MentionFilters {
  const list = (v: unknown) => (Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : [])
  const r = (raw && typeof raw === 'object' ? raw : {}) as Record<string, unknown>
  return { reasons: list(r.reasons), accounts: list(r.accounts), repos: list(r.repos) }
}

export interface MentionFilterOption {
  value: string
  label: string
  count: number
}

/** Options per dimension with counts, from the current items (most first). */
export function filterOptions(items: MentionItem[]): Record<MentionFilterDimension, MentionFilterOption[]> {
  const tally = (pick: (i: MentionItem) => [string, string]) => {
    const m = new Map<string, MentionFilterOption>()
    for (const i of items) {
      const [value, label] = pick(i)
      const o = m.get(value) ?? { value, label, count: 0 }
      o.count++
      m.set(value, o)
    }
    return [...m.values()].sort((a, b) => b.count - a.count || a.label.localeCompare(b.label))
  }
  return {
    reasons: tally(i => [i.reason, i.reason]),
    accounts: tally(i => [i.account_id, i.account_label || i.account_login]),
    repos: tally(i => [repoOf(i), repoOf(i)]),
  }
}

/** Selected values that still match an option (stale ones are ignored). */
export function activeFilters(filters: MentionFilters, items: MentionItem[]): MentionFilters {
  const opts = filterOptions(items)
  const keep = (d: MentionFilterDimension) => filters[d].filter(v => opts[d].some(o => o.value === v))
  return { reasons: keep('reasons'), accounts: keep('accounts'), repos: keep('repos') }
}

export function applyFilters(items: MentionItem[], filters: MentionFilters, search: string): MentionItem[] {
  const q = search.trim().toLowerCase()
  return items.filter(i =>
    (filters.reasons.length === 0 || filters.reasons.includes(i.reason))
    && (filters.accounts.length === 0 || filters.accounts.includes(i.account_id))
    && (filters.repos.length === 0 || filters.repos.includes(repoOf(i)))
    && (!q || [i.title, repoOf(i), i.trigger?.author_login ?? '', i.trigger?.body ?? '', i.number ? `#${i.number}` : '']
      .some(s => s.toLowerCase().includes(q))),
  )
}

export interface MentionGroup {
  key: string
  label: string
  /** Avatar for person groups. */
  avatarUrl: string | null
  items: MentionItem[]
}

/** Pending mentions grouped (order of first appearance = newest first). */
export function groupMentions(items: MentionItem[], by: MentionGroupBy): MentionGroup[] {
  if (by === 'none') return items.length ? [{ key: 'all', label: '', avatarUrl: null, items }] : []
  const groups = new Map<string, MentionGroup>()
  for (const i of items) {
    const key = by === 'repo' ? repoOf(i).toLowerCase() : (i.trigger?.author_login ?? '').toLowerCase()
    const label = by === 'repo' ? repoOf(i) : (i.trigger?.author_login ?? '')
    const g = groups.get(key) ?? { key, label, avatarUrl: by === 'person' ? (i.trigger?.author_avatar_url ?? null) : null, items: [] }
    g.items.push(i)
    groups.set(key, g)
  }
  return [...groups.values()]
}

/** Split into waiting (newest mention first) and answered (newest answer first). */
export function splitAnswered(items: MentionItem[]): { pending: MentionItem[]; answered: MentionItem[] } {
  const pending = items.filter(i => !i.answered).sort((a, b) => mentionedAt(b).localeCompare(mentionedAt(a)))
  const answered = items.filter(i => i.answered)
    .sort((a, b) => (b.answered_at ?? '').localeCompare(a.answered_at ?? ''))
  return { pending, answered }
}

/**
 * "Check with AI" button state: `unavailable` (a discussion — no issue / PR
 * to open a session on), `unmapped` (the repo is not in a Devdy project),
 * `none` (can start), `running` / `done` (a session exists — opens it).
 */
export type MentionCheckStatus = 'unavailable' | 'unmapped' | 'none' | 'running' | 'done'

export function checkStatusOf(item: MentionItem, runStatus: string | null): MentionCheckStatus {
  if (item.existing_run_id) return runStatus === 'running' || runStatus === 'fetched' ? 'running' : 'done'
  if (item.number === null || (item.kind !== 'PullRequest' && item.kind !== 'Issue')) return 'unavailable'
  if (!item.project_id || !item.repo_id) return 'unmapped'
  return 'none'
}

/** The trigger comment as a Markdown quote for the session's first message. */
export function quoteExcerpt(body: string): string {
  const text = body.trim()
  return text ? text.split(/\r?\n/).map(l => `> ${l}`).join('\n') : ''
}
