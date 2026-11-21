// The main window's tab strip as data: its entries, the split pair (two tabs
// shown side by side, Chrome-style) and the split ratio — what
// stores/workspaceTabs keeps and persists, as pure functions.
//
// Pure logic only — no Vue / router / Tauri imports — so it is unit-tested
// directly (src/lib/__tests__/workspaceState.test.mjs).
import { cleanParams, isSurfaceKind, surfaceTabId, type SurfaceKind, type SurfaceParams } from './surfaces.ts'

/**
 * A project pinned as a tab in the workspace. Switching between runs inside a
 * project is still done from RunView's History list, not tabs. Each tab
 * remembers the last run the user was viewing in that project so re-selecting
 * the tab returns there.
 */
export interface WorkspaceTab {
  projectId: string
  lastRunId: string | null
}

/**
 * A surface (PR Inbox, Calendar, Slack threads, Todos, a file…) opened as a tab
 * of the main window instead of its own OS window (see lib/surfaces).
 */
export interface SurfaceTab {
  id: string
  surface: SurfaceKind
  params: SurfaceParams
}

/**
 * A page of the main window (Dashboard, Skills, Settings… — lib/navigation
 * `pageRouteFor`) as a tab. Remembers its last full path (query included), so
 * coming back lands where the user was.
 */
export interface PageTab {
  id: string
  path: string
  fullPath: string
}

/** One entry of the shared tab strip, in display order. */
export type TabEntry =
  | ({ type: 'project' } & WorkspaceTab)
  | ({ type: 'surface' } & SurfaceTab)
  | ({ type: 'page' } & PageTab)

/**
 * The two tabs of a split view, by `tabEntryKey`: `left` is what the router
 * shows, `right` is rendered beside it. They always sit next to each other on
 * the strip, left first.
 */
export interface SplitPair {
  left: string
  right: string
}

export type PaneSide = 'left' | 'right'

export function pageTabId(path: string): string {
  return `page:${path}`
}

export function tabEntryKey(e: TabEntry): string {
  return e.type === 'project' ? `project:${e.projectId}` : e.id
}

/**
 * Whether two tabs may share a split. Never a tab with itself, and never two
 * sessions: a session screen owns app-wide singletons (the conductor panel,
 * window key / drop listeners) that two of them would fight over.
 */
export function canPair(a: TabEntry, b: TabEntry): boolean {
  if (tabEntryKey(a) === tabEntryKey(b)) return false
  return !(a.type === 'project' && b.type === 'project')
}

// --- Split ratio --------------------------------------------------------------

export const DEFAULT_SPLIT_RATIO = 0.5
export const SPLIT_MIN_LEFT = 360
export const SPLIT_MIN_RIGHT = 320

/** A share (0–1) of the left pane, kept sane for storage. */
export function sanitizeRatio(v: unknown): number {
  return typeof v === 'number' && Number.isFinite(v) && v > 0.05 && v < 0.95 ? v : DEFAULT_SPLIT_RATIO
}

/**
 * The left pane's share of `total` px once both panes keep their minimum
 * width. When both minimums don't fit, they shrink proportionally.
 */
export function clampRatio(
  ratio: number,
  total: number,
  minLeft = SPLIT_MIN_LEFT,
  minRight = SPLIT_MIN_RIGHT,
): number {
  const r = sanitizeRatio(ratio)
  if (!(total > 0)) return r
  if (minLeft + minRight >= total) return minLeft / (minLeft + minRight)
  return Math.min(1 - minRight / total, Math.max(minLeft / total, r))
}

// --- Strip order --------------------------------------------------------------

/** Move `right` to sit right after `left` (the pair is drawn as one tab). */
export function placePair(keys: readonly string[], pair: SplitPair): string[] {
  if (!keys.includes(pair.left) || !keys.includes(pair.right)) return [...keys]
  const out = keys.filter((k) => k !== pair.right)
  out.splice(out.indexOf(pair.left) + 1, 0, pair.right)
  return out
}

/**
 * The strip as drawable units: one per tab, except the pair which is one unit
 * of its two adjacent tabs. Each unit lists its indices into `keys`.
 */
export function stripUnits(keys: readonly string[], pair: SplitPair | null): number[][] {
  const units: number[][] = []
  for (let i = 0; i < keys.length; i++) {
    if (pair && keys[i] === pair.left && keys[i + 1] === pair.right) {
      units.push([i, i + 1])
      i++
    } else {
      units.push([i])
    }
  }
  return units
}

/** Drag-and-drop on the strip: move unit `from` to unit slot `to` (the pair moves as one). */
export function moveUnit(keys: readonly string[], pair: SplitPair | null, from: number, to: number): string[] {
  const units = stripUnits(keys, pair)
  if (from === to || !units[from] || !units[to]) return [...keys]
  const groups = units.map((u) => u.map((i) => keys[i]))
  const [moved] = groups.splice(from, 1)
  groups.splice(to, 0, moved)
  return groups.flat()
}

/**
 * Put `key` in one half of the pair (navigation inside that half landed on
 * another tab). It moves into that half's place on the strip; the tab it
 * replaces stays open, right after the pair. Returns the new order and pair, or
 * null when it can't (it is the other half already).
 */
export function replacePairSide(
  keys: readonly string[],
  pair: SplitPair,
  side: PaneSide,
  key: string,
): { keys: string[]; pair: SplitPair } | null {
  const other = side === 'left' ? pair.right : pair.left
  if (key === other) return null
  const old = pair[side]
  if (key === old) return { keys: placePair(keys, pair), pair }
  const next: SplitPair = side === 'left' ? { left: key, right: pair.right } : { left: pair.left, right: key }
  const out = keys.filter((k) => k !== key && k !== old)
  const at = out.indexOf(other)
  if (at === -1) return null
  // [left, right], then the replaced tab.
  out.splice(side === 'left' ? at : at + 1, 0, key)
  if (keys.includes(old)) out.splice(out.indexOf(next.right) + 1, 0, old)
  return { keys: out, pair: next }
}

// --- Persistence ----------------------------------------------------------------

export const WORKSPACE_STATE_VERSION = 2

export interface WorkspaceState {
  entries: TabEntry[]
  /** Kept across restarts, but not shown until the user clicks the pair. */
  pair: SplitPair | null
  ratio: number
}

function parseEntry(t: unknown): TabEntry | null {
  if (!t || typeof t !== 'object') return null
  const o = t as Record<string, unknown>
  // Entries saved before surface tabs existed have no `type`: projects.
  if ((o.type === 'project' || o.type === undefined) && typeof o.projectId === 'string') {
    return { type: 'project', projectId: o.projectId, lastRunId: typeof o.lastRunId === 'string' ? o.lastRunId : null }
  }
  if (o.type === 'page' && typeof o.path === 'string' && o.path.startsWith('/')) {
    const fullPath = typeof o.fullPath === 'string' && o.fullPath.startsWith(o.path) ? o.fullPath : o.path
    return { type: 'page', id: pageTabId(o.path), path: o.path, fullPath }
  }
  if (o.type === 'surface' && isSurfaceKind(o.surface)) {
    const params = cleanParams(o.surface, o.params as Record<string, unknown> | undefined)
    return { type: 'surface', id: surfaceTabId(o.surface, params), surface: o.surface, params }
  }
  return null
}

/**
 * Read the persisted strip. Version 1 was a bare array of entries (the split
 * was never saved); version 2 is `{ version, entries, pair, ratio }`. Anything
 * malformed is dropped rather than failing the whole strip.
 */
export function parseWorkspaceState(raw: string | null | undefined): WorkspaceState {
  const empty: WorkspaceState = { entries: [], pair: null, ratio: DEFAULT_SPLIT_RATIO }
  if (!raw) return empty
  let data: unknown
  try {
    data = JSON.parse(raw)
  } catch {
    return empty
  }
  const list: unknown[] | null = Array.isArray(data)
    ? data
    : data && typeof data === 'object' && Array.isArray((data as { entries?: unknown }).entries)
      ? (data as { entries: unknown[] }).entries
      : null
  if (!list) return empty
  const entries: TabEntry[] = []
  const seen = new Set<string>()
  for (const t of list) {
    const e = parseEntry(t)
    if (!e || seen.has(tabEntryKey(e))) continue
    seen.add(tabEntryKey(e))
    entries.push(e)
  }
  const obj = Array.isArray(data) ? {} : (data as Record<string, unknown>)
  const ratio = sanitizeRatio(obj.ratio)
  let pair: SplitPair | null = null
  const p = obj.pair as Record<string, unknown> | null | undefined
  if (p && typeof p.left === 'string' && typeof p.right === 'string') {
    const l = entries.find((e) => tabEntryKey(e) === p.left)
    const r = entries.find((e) => tabEntryKey(e) === p.right)
    if (l && r && canPair(l, r)) pair = { left: p.left, right: p.right }
  }
  if (pair) {
    const order = placePair(entries.map(tabEntryKey), pair)
    entries.sort((a, b) => order.indexOf(tabEntryKey(a)) - order.indexOf(tabEntryKey(b)))
  }
  return { entries, pair, ratio }
}

export function serializeWorkspaceState(s: WorkspaceState): string {
  return JSON.stringify({ version: WORKSPACE_STATE_VERSION, entries: s.entries, pair: s.pair, ratio: s.ratio })
}

// --- Routes → tabs --------------------------------------------------------------

/** The tab a route location belongs to, as data (no store needed). */
export type EntryTarget =
  | { type: 'project'; projectId: string; runId: string | null }
  | { type: 'surface'; surface: SurfaceKind; params: SurfaceParams }
  | { type: 'page'; path: string; fullPath: string }

export interface RouteLike {
  name?: unknown
  path: string
  fullPath: string
  params: Record<string, unknown>
  query: Record<string, unknown>
}

/**
 * Which tab a route shows: a session (run routes only), a surface, or a page
 * (`isPage` — lib/navigation `pageRouteFor`). Null for screens that aren't tabs
 * (a project's settings…).
 */
export function entryTargetFor(
  route: RouteLike,
  surfaceFor: (path: string) => SurfaceKind | null,
  isPage: (path: string) => boolean,
): EntryTarget | null {
  const { name, params } = route
  if ((name === 'project-run' || name === 'project-run-detail') && typeof params.projectId === 'string') {
    return { type: 'project', projectId: params.projectId, runId: typeof params.runId === 'string' ? params.runId : null }
  }
  const kind = surfaceFor(route.path)
  if (kind) return { type: 'surface', surface: kind, params: cleanParams(kind, route.query) }
  if (isPage(route.path)) return { type: 'page', path: route.path, fullPath: route.fullPath }
  return null
}

export function entryTargetKey(t: EntryTarget): string {
  if (t.type === 'project') return `project:${t.projectId}`
  if (t.type === 'surface') return surfaceTabId(t.surface, t.params)
  return pageTabId(t.path)
}
