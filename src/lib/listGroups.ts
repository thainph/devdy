// Pure helpers behind the list-detail screens (Servers, Rules, Skills, MCP):
// search matching, grouping rows into collapsible sidebar folders, and the
// remembered collapsed-folder set.

/** Key of the catch-all group (rows with no group key). */
export const GROUP_NONE = '__none__'

export interface RowGroup<T> {
  key: string
  label: string
  rows: T[]
}

/** A flat-list sort a sidebar offers next to its default, grouped order. */
export interface ListSort<T> {
  value: string
  label: string
  compare: (a: T, b: T) => number
}

/** A–Z by the given text, empty ones last. */
export function byText<T>(textOf: (row: T) => string): (a: T, b: T) => number {
  return (a, b) => {
    const x = textOf(a)
    const y = textOf(b)
    return (!x ? 1 : 0) - (!y ? 1 : 0) || x.localeCompare(y)
  }
}

/** Newest first by an ISO time (missing ones last). */
export function byNewest<T>(timeOf: (row: T) => string | null | undefined): (a: T, b: T) => number {
  return (a, b) => {
    const x = timeOf(a) ?? ''
    const y = timeOf(b) ?? ''
    return x < y ? 1 : x > y ? -1 : 0
  }
}

/** Lower-cased haystack from the given parts (null/undefined skipped). */
export function haystack(...parts: Array<string | number | null | undefined>): string {
  return parts
    .filter((p) => p !== null && p !== undefined && p !== '')
    .map((p) => String(p))
    .join('\n')
    .toLowerCase()
}

/** True when every whitespace-separated term of `query` occurs in `hay`. */
export function matchesSearch(hay: string, query: string): boolean {
  const terms = query.trim().toLowerCase().split(/\s+/).filter(Boolean)
  return terms.every((term) => hay.includes(term))
}

/**
 * Groups rows by `keyOf` (null → the catch-all group). Groups sort A–Z by
 * label with the catch-all last; rows keep their input order inside a group.
 */
export function groupRows<T>(
  rows: readonly T[],
  keyOf: (row: T) => string | null,
  labelOf: (key: string) => string,
  noneLabel: string,
): RowGroup<T>[] {
  const map = new Map<string, T[]>()
  for (const row of rows) {
    const key = keyOf(row) ?? GROUP_NONE
    const list = map.get(key)
    if (list) list.push(row)
    else map.set(key, [row])
  }
  return [...map.entries()]
    .map(([key, list]) => ({
      key,
      label: key === GROUP_NONE ? noneLabel : labelOf(key),
      rows: list,
    }))
    .sort((a, b) =>
      a.key === GROUP_NONE ? 1 : b.key === GROUP_NONE ? -1 : a.label.localeCompare(b.label),
    )
}

/** Splits a comma-separated tag string into trimmed, de-duplicated tags. */
export function splitTags(tags: string | null | undefined): string[] {
  const out: string[] = []
  for (const raw of (tags ?? '').split(',')) {
    const tag = raw.trim()
    if (tag && !out.includes(tag)) out.push(tag)
  }
  return out
}

/** Reads a remembered set of collapsed group keys (bad/missing data → empty). */
export function parseCollapsed(raw: string | null): Set<string> {
  try {
    const parsed = JSON.parse(raw ?? '[]')
    return new Set(Array.isArray(parsed) ? parsed.filter((x) => typeof x === 'string') : [])
  } catch {
    return new Set()
  }
}
