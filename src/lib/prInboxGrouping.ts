// Pure grouping/filtering for the PR inbox (PrInboxView). Kept free of Vue and
// i18n so it can be unit-tested with `node --test src/lib/__tests__/`; the view
// translates the special groups' labels from their `kind`.
import type { PrInboxItem } from '@/stores/prInbox'
import { reviewerAccountsOf } from './prInboxFilters.ts'

export type PrGroupBy = 'none' | 'project' | 'repo' | 'author' | 'reviewer_account'
export const PR_GROUP_BY_OPTIONS: readonly PrGroupBy[] = ['none', 'project', 'repo', 'author', 'reviewer_account']

/**
 * - `all`:      the single headerless bucket of `none` mode
 * - `normal`:   a project / repo / author / reviewer-account group
 * - `unmapped`: PRs whose repo isn't tracked in devdy (project mode only)
 * - `bots`:     every bot-authored PR when bots aren't hidden
 */
export type PrGroupKind = 'all' | 'normal' | 'unmapped' | 'bots'

export interface PrGroup {
  /** Stable key — also used to persist the collapsed state. Namespaced by the
   * group-by mode (`project:…`, `repo:…`), special groups included, so a
   * collapse choice in one mode doesn't leak into another. */
  key: string
  kind: PrGroupKind
  /** Display name for `normal` groups; empty for special kinds (and for a
   * blank author login — the view shows a fallback). */
  label: string
  /** Author avatar (author mode only). */
  avatarUrl: string | null
  items: PrInboxItem[]
  /** Mapped PRs without a linked review run — the ones still actionable. */
  unreviewedCount: number
}

export interface GroupedPrInbox {
  groups: PrGroup[]
  /** Bot PRs left out because `hideBots` is on. */
  hiddenBotCount: number
  /** Distinct visible PRs — in reviewer-account mode a PR requested from two
   * accounts sits in two groups but counts once here. */
  visibleCount: number
  totalCount: number
}

// Special-group suffixes; the full key is `<groupBy>:<suffix>` (see PrGroup.key).
export const UNMAPPED_GROUP_KEY = '__unmapped__'
export const BOTS_GROUP_KEY = '__bots__'
export const ALL_GROUP_KEY = '__all__'

function specialKey(groupBy: PrGroupBy, suffix: string): string {
  return `${groupBy}:${suffix}`
}

function byUpdatedDesc(a: PrInboxItem, b: PrInboxItem): number {
  return a.updated_at < b.updated_at ? 1 : a.updated_at > b.updated_at ? -1 : 0
}

function makeGroup(key: string, kind: PrGroupKind, label: string, items: PrInboxItem[], avatarUrl: string | null = null): PrGroup {
  return {
    key,
    kind,
    label,
    avatarUrl,
    items,
    unreviewedCount: items.filter(i => i.mapped && i.existing_run_id == null).length,
  }
}

interface Bucket { key: string; kind: PrGroupKind; label: string }

/** Group key(s) + label(s) for a PR under the given mode (`none` excluded).
 * Only reviewer-account mode can return several — one per requested account. */
function bucketsOf(item: PrInboxItem, groupBy: Exclude<PrGroupBy, 'none'>): Bucket[] {
  switch (groupBy) {
    case 'project':
      return [item.mapped && item.project_id
        // A blank project name would render an empty header — fall back to the id.
        ? { key: `project:${item.project_id}`, kind: 'normal', label: item.project_name?.trim() || item.project_id }
        : { key: specialKey('project', UNMAPPED_GROUP_KEY), kind: 'unmapped', label: '' }]
    case 'repo': {
      const name = `${item.owner}/${item.repo}`
      return [{ key: `repo:${name.toLowerCase()}`, kind: 'normal', label: name }]
    }
    case 'author': {
      const login = item.author_login.trim()
      return [{ key: `author:${login.toLowerCase()}`, kind: 'normal', label: login }]
    }
    case 'reviewer_account': {
      const seen = new Set<string>()
      return reviewerAccountsOf(item)
        .filter(a => !seen.has(a.account_id) && seen.add(a.account_id))
        .map(a => ({
          key: `reviewer_account:${a.account_id}`,
          kind: 'normal' as const,
          label: a.account_label.trim() || a.account_id,
        }))
    }
  }
}

/**
 * Split the inbox into ordered groups. Within a group PRs stay newest-first;
 * groups are ordered by their newest PR, with "Not in devdy" and "Bots" always
 * last. Bot PRs are either hidden (counted in `hiddenBotCount`) or gathered
 * into one "Bots" group — except in `none` mode, where they stay inline.
 * `isBot` defaults to the backend's `author_is_bot` (the view also honours the
 * user's custom bot list, see prInboxFilters.makeBotPredicate).
 */
export function groupPrInbox(
  items: readonly PrInboxItem[],
  opts: { groupBy: PrGroupBy; hideBots: boolean; isBot?: (item: PrInboxItem) => boolean },
): GroupedPrInbox {
  const isBot = opts.isBot ?? ((i: PrInboxItem) => i.author_is_bot)
  const sorted = [...items].sort(byUpdatedDesc)
  const bots = sorted.filter(i => isBot(i))
  const humans = sorted.filter(i => !isBot(i))
  const hiddenBotCount = opts.hideBots ? bots.length : 0
  const visibleCount = sorted.length - hiddenBotCount
  const result = (groups: PrGroup[]): GroupedPrInbox => ({
    groups,
    hiddenBotCount,
    visibleCount,
    totalCount: sorted.length,
  })

  if (opts.groupBy === 'none') {
    const visible = opts.hideBots ? humans : sorted
    return result(visible.length > 0 ? [makeGroup(specialKey('none', ALL_GROUP_KEY), 'all', '', visible)] : [])
  }

  // Map preserves insertion order, and `humans` is newest-first, so groups come
  // out ordered by their newest PR without a second sort.
  const buckets = new Map<string, { kind: PrGroupKind; label: string; items: PrInboxItem[] }>()
  for (const item of humans) {
    for (const b of bucketsOf(item, opts.groupBy)) {
      const existing = buckets.get(b.key)
      if (existing) existing.items.push(item)
      else buckets.set(b.key, { kind: b.kind, label: b.label, items: [item] })
    }
  }

  const groups: PrGroup[] = []
  let unmapped: PrGroup | null = null
  for (const [key, b] of buckets) {
    const avatar = opts.groupBy === 'author' ? (b.items.find(i => i.author_avatar_url)?.author_avatar_url ?? null) : null
    const group = makeGroup(key, b.kind, b.label, b.items, avatar)
    if (b.kind === 'unmapped') unmapped = group
    else groups.push(group)
  }
  if (unmapped) groups.push(unmapped)
  if (!opts.hideBots && bots.length > 0) groups.push(makeGroup(specialKey(opts.groupBy, BOTS_GROUP_KEY), 'bots', '', bots))
  return result(groups)
}

/** Default collapsed state when the user hasn't toggled a group yet. */
export function isGroupCollapsedByDefault(group: Pick<PrGroup, 'kind'>): boolean {
  return group.kind === 'bots'
}
