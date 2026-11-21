// The view of the list-detail sidebars of Skills, Rules, MCP and Servers
// (components/listDetail/ListDetailSidebar): what the search box's filter menu
// sets — one choice per host filter section ('' = all), a sort and whether the
// rows sit in folders — its defaults, the "Filtering: …" summary, and the
// remembered display (sort + folders). Pure, so it is testable.

/** One choice of a filter section ('' is the implicit "All" the sidebar adds). */
export interface ListFilterOption {
  value: string
  label: string
  count?: number | null
}

/** A filter section of the menu (e.g. "Group", "Type", "Tag"). */
export interface ListFilterDef {
  key: string
  title: string
  options: ListFilterOption[]
}

/** Section key → chosen value ('' or absent = all). */
export type ListFilterValues = Record<string, string>

/** The part of the view remembered between sessions (filters are not). */
export interface ListDisplay {
  /** A host sort value, or '' when the host offers none. */
  sort: string
  /** Rows under one header per folder. */
  grouped: boolean
}

/** Any filter narrows the list (what the summary line and "Clear filters" are about). */
export function isFilteringList(values: ListFilterValues): boolean {
  return Object.values(values).some(Boolean)
}

export function clearListFilters(values: ListFilterValues): ListFilterValues {
  return Object.fromEntries(Object.keys(values).map((k) => [k, '']))
}

/** Anything differs from the defaults: the filter button shows its accent dot. */
export function listViewDiffers(values: ListFilterValues, display: ListDisplay, defaults: ListDisplay): boolean {
  return isFilteringList(values) || display.sort !== defaults.sort || display.grouped !== defaults.grouped
}

/** "Design · stdio": labels of the chosen options in menu order, empty when not filtering. */
export function listFilterSummary(defs: readonly ListFilterDef[], values: ListFilterValues): string[] {
  const parts: string[] = []
  for (const def of defs) {
    const v = values[def.key]
    if (v) parts.push(def.options.find((o) => o.value === v)?.label ?? v)
  }
  return parts
}

/**
 * Drops choices no longer offered (a deleted group, a tag whose last server
 * went, a section the host stopped showing). Returns null when nothing changed.
 */
export function pruneListFilters(defs: readonly ListFilterDef[], values: ListFilterValues): ListFilterValues | null {
  let changed = false
  const next: ListFilterValues = {}
  for (const [key, v] of Object.entries(values)) {
    const def = defs.find((d) => d.key === key)
    const keep = !v || !!def?.options.some((o) => o.value === v)
    if (!keep) changed = true
    next[key] = keep ? v : ''
  }
  return changed ? next : null
}

/**
 * Reads a remembered display, keeping only valid values. `legacyOrder` is the
 * older single "order" setting: '' meant the folders, a sort value a flat list
 * by that sort.
 */
export function parseListDisplay(
  raw: string | null,
  legacyOrder: string | null,
  sorts: readonly string[],
  defaults: ListDisplay,
): ListDisplay {
  const validSort = (s: unknown) => typeof s === 'string' && sorts.includes(s)
  let o: Record<string, unknown> | null = null
  try {
    const parsed = raw ? JSON.parse(raw) : null
    if (parsed && typeof parsed === 'object') o = parsed as Record<string, unknown>
  } catch {
    /* bad data: defaults */
  }
  if (o) {
    return {
      sort: validSort(o.sort) ? (o.sort as string) : defaults.sort,
      grouped: typeof o.grouped === 'boolean' ? o.grouped : defaults.grouped,
    }
  }
  if (legacyOrder === '') return { ...defaults, grouped: true }
  if (validSort(legacyOrder)) return { sort: legacyOrder!, grouped: false }
  return { ...defaults }
}
