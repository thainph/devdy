// The Todos / Notes sidebar's view: what the search box's filter menu sets
// (status, project, sort, group by project, show done) — its defaults, which
// rows pass it, the per-status counts the menu shows, and the "Filtering: …"
// summary line. Pure, so the sidebar and the screen agree and it is testable.

export type ItemListKind = 'note' | 'todo'

/**
 * '' = every item. Todos: a lateness status (lib/focusRadar); notes: 'linked'
 * (linked to a session).
 */
export type ItemStatusFilter = '' | 'overdue' | 'today' | 'due-soon' | 'waiting' | 'linked'

/** Todos: 'urgency' (most late first) or 'due'; notes: 'updated' or 'title'. */
export type ItemSortMode = 'urgency' | 'due' | 'updated' | 'title'

export interface ItemListView {
  status: ItemStatusFilter
  /** '' = every project, ITEM_NO_PROJECT = items without one, else a project id. */
  project: string
  sort: ItemSortMode
  /** Rows under one header per project. */
  grouped: boolean
  /** Todos: done ones are listed too. */
  showDone: boolean
}

export const ITEM_NO_PROJECT = '__none__'

export const TODO_STATUSES: ItemStatusFilter[] = ['', 'overdue', 'today', 'due-soon', 'waiting']
export const NOTE_STATUSES: ItemStatusFilter[] = ['', 'linked']

export function itemStatuses(kind: ItemListKind): ItemStatusFilter[] {
  return kind === 'todo' ? TODO_STATUSES : NOTE_STATUSES
}

export function itemSorts(kind: ItemListKind): ItemSortMode[] {
  return kind === 'todo' ? ['urgency', 'due'] : ['updated', 'title']
}

export function defaultItemListView(kind: ItemListKind): ItemListView {
  return { status: '', project: '', sort: kind === 'todo' ? 'urgency' : 'updated', grouped: true, showDone: false }
}

/** The part of the view remembered between sessions (filters are not). */
export type ItemListDisplay = Pick<ItemListView, 'sort' | 'grouped' | 'showDone'>

/** Read a remembered display, keeping only values valid for the kind. */
export function parseItemListDisplay(kind: ItemListKind, raw: unknown): ItemListDisplay {
  const d = defaultItemListView(kind)
  const o = raw && typeof raw === 'object' ? (raw as Record<string, unknown>) : {}
  return {
    sort: itemSorts(kind).includes(o.sort as ItemSortMode) ? (o.sort as ItemSortMode) : d.sort,
    grouped: typeof o.grouped === 'boolean' ? o.grouped : d.grouped,
    showDone: kind === 'todo' && typeof o.showDone === 'boolean' ? o.showDone : d.showDone,
  }
}

/** Status or project narrows the list (what the summary line and "Clear filters" are about). */
export function isFiltering(view: ItemListView): boolean {
  return !!view.status || !!view.project
}

/** Anything differs from the defaults: the filter button shows its accent dot. */
export function viewDiffersFromDefault(kind: ItemListKind, view: ItemListView): boolean {
  const d = defaultItemListView(kind)
  return (
    isFiltering(view) ||
    view.sort !== d.sort ||
    view.grouped !== d.grouped ||
    (kind === 'todo' && view.showDone !== d.showDone)
  )
}

export function clearItemFilters(view: ItemListView): ItemListView {
  return { ...view, status: '', project: '' }
}

/** The fields of a sidebar row the view looks at. */
export interface ItemViewRow {
  id: string
  projectId: string | null
  done: boolean
  linkedRunCount: number
}

/** Lateness status of each open todo by id (lib/focusRadar `radar`); absent = unknown / done. */
export type RadarById = Record<string, string | undefined>

export function inItemProject(row: Pick<ItemViewRow, 'projectId'>, project: string): boolean {
  if (!project) return true
  if (project === ITEM_NO_PROJECT) return !row.projectId
  return row.projectId === project
}

function inScope(kind: ItemListKind, row: ItemViewRow, view: ItemListView): boolean {
  return inItemProject(row, view.project) && (kind !== 'todo' || view.showDone || !row.done)
}

function hasStatus(row: ItemViewRow, status: ItemStatusFilter, radar: RadarById | null): boolean {
  if (!status) return true
  if (status === 'linked') return row.linkedRunCount > 0
  return radar?.[row.id] === status
}

export function matchesItemView(kind: ItemListKind, row: ItemViewRow, view: ItemListView, radar: RadarById | null): boolean {
  return inScope(kind, row, view) && hasStatus(row, view.status, radar)
}

/** Counts behind each status of the filter menu: the project in scope, before the status filter. */
export function itemStatusCounts(
  kind: ItemListKind,
  rows: ItemViewRow[],
  view: ItemListView,
  radar: RadarById | null,
): Record<ItemStatusFilter, number> {
  const counts = { '': 0, overdue: 0, today: 0, 'due-soon': 0, waiting: 0, linked: 0 } as Record<ItemStatusFilter, number>
  for (const row of rows) {
    if (!inScope(kind, row, view)) continue
    counts['']++
    for (const s of itemStatuses(kind)) if (s && hasStatus(row, s, radar)) counts[s]++
  }
  return counts
}

/** "Overdue · icontruction": the parts of the "Filtering:" line, empty when not filtering. */
export function itemFilterSummary(
  view: ItemListView,
  labels: { status: (s: ItemStatusFilter) => string; project: (id: string) => string },
): string[] {
  const parts: string[] = []
  if (view.status) parts.push(labels.status(view.status))
  if (view.project) parts.push(labels.project(view.project))
  return parts
}
