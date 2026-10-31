// Pure helpers behind the Rules list-detail screen: the sidebar's filtering
// and group folders, and the rule file's frontmatter (the content stays the
// source of truth for name / description / target / codex_mode).
import { GROUP_NONE, haystack, matchesSearch, type RowGroup } from './listGroups.ts'

export type RuleTargetValue = 'claude' | 'codex' | 'both'
export type CodexMode = 'inline' | 'skill'

/** Group-filter value for rules in no group ('' = all groups). */
export const UNGROUPED = '__ungrouped__'

export interface RuleLike {
  id: string
  name: string
  description: string
  target: RuleTargetValue
}

export interface GroupLike {
  id: string
  name: string
}

export interface RuleRow<R extends RuleLike = RuleLike> {
  /** The rule id (a rule in several groups has one row per group folder). */
  id: string
  rule: R
  /** The folder this row sits in (GROUP_NONE = ungrouped). */
  groupKey: string
  /** The rule's other groups, shown as badges on the row. */
  otherGroupIds: string[]
}

/**
 * Filters the rules and lays them out in group folders, in the groups' own
 * order with "Ungrouped" last. A rule in several groups appears under each
 * of them — membership is many-to-many, and hiding it from one of its groups
 * would make that folder look incomplete. Empty folders are dropped.
 */
export function buildRuleGroups<R extends RuleLike>(
  rules: readonly R[],
  opts: {
    groups: readonly GroupLike[]
    /** rule id → group ids. */
    itemGroups: Record<string, string[] | undefined>
    /** rule id → file content, when known (searched too). */
    contents?: Record<string, string | undefined>
    search: string
    /** '' = all, UNGROUPED, or a group id. */
    groupFilter: string
    noneLabel: string
  },
): RowGroup<RuleRow<R>>[] {
  const known = new Map(opts.groups.map((g) => [g.id, g]))
  const buckets = new Map<string, RuleRow<R>[]>()
  for (const rule of rules) {
    const hay = haystack(rule.name, rule.description, opts.contents?.[rule.id])
    if (!matchesSearch(hay, opts.search)) continue
    // Stale ids (a group deleted since) don't count as membership.
    const ids = (opts.itemGroups[rule.id] ?? []).filter((id) => known.has(id))
    const keys = ids.length > 0 ? ids : [GROUP_NONE]
    for (const key of keys) {
      if (opts.groupFilter === UNGROUPED && key !== GROUP_NONE) continue
      if (opts.groupFilter && opts.groupFilter !== UNGROUPED && key !== opts.groupFilter) continue
      const row: RuleRow<R> = { id: rule.id, rule, groupKey: key, otherGroupIds: ids.filter((id) => id !== key) }
      const list = buckets.get(key)
      if (list) list.push(row)
      else buckets.set(key, [row])
    }
  }
  const order = [...opts.groups.map((g) => g.id), GROUP_NONE]
  return order
    .filter((key) => buckets.has(key))
    .map((key) => ({
      key,
      label: key === GROUP_NONE ? opts.noneLabel : known.get(key)!.name,
      rows: buckets.get(key)!,
    }))
}

// ── Frontmatter ─────────────────────────────────────────────────────────────

export interface RuleFrontmatter {
  name: string
  description: string
  target: string
  codexMode: string
}

export function parseFrontmatter(raw: string): RuleFrontmatter {
  const empty = { name: '', description: '', target: '', codexMode: '' }
  if (!raw.startsWith('---')) return empty
  const rest = raw.slice(3)
  const end = rest.indexOf('\n---')
  if (end === -1) return empty
  const fm = rest.slice(0, end)
  const pick = (key: string) =>
    fm.match(new RegExp(`^${key}:\\s*(.+)$`, 'm'))?.[1]?.trim().replace(/^["']|["']$/g, '') ?? ''
  return { name: pick('name'), description: pick('description'), target: pick('target'), codexMode: pick('codex_mode') }
}

export type FrontmatterError =
  | 'errMissingName'
  | 'errNameSlug'
  | 'errMissingDescription'
  | 'errTargetInvalid'
  | 'errCodexModeInvalid'

/** The first problem with the frontmatter (an i18n key under rules.editor), or null. */
export function validateFrontmatter(raw: string): FrontmatterError | null {
  const fm = parseFrontmatter(raw)
  if (!fm.name) return 'errMissingName'
  if (!/^[a-zA-Z0-9_-]+$/.test(fm.name)) return 'errNameSlug'
  if (!fm.description) return 'errMissingDescription'
  if (fm.target && !['claude', 'codex', 'both'].includes(fm.target)) return 'errTargetInvalid'
  if (fm.codexMode && !['inline', 'skill'].includes(fm.codexMode)) return 'errCodexModeInvalid'
  return null
}

/**
 * Rewrites (or appends) one frontmatter line; `null` drops it. Content without
 * a frontmatter block comes back unchanged.
 */
export function setFrontmatterField(raw: string, key: string, value: string | null): string {
  if (!raw.startsWith('---')) return raw
  const rest = raw.slice(3)
  const end = rest.indexOf('\n---')
  if (end === -1) return raw
  let fm = rest.slice(0, end)
  const after = rest.slice(end)
  const line = new RegExp(`^${key}:.*$`, 'm')
  if (value === null) {
    fm = fm.replace(new RegExp(`\\n${key}:.*$`, 'm'), '')
  } else if (line.test(fm)) {
    fm = fm.replace(line, `${key}: ${value}`)
  } else {
    fm = fm.replace(/\n*$/, '') + `\n${key}: ${value}`
  }
  return '---' + fm + after
}
