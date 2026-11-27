// The main window's tab strip as data: its entries, the split view (on / off,
// the session on the left, the screen on the right) and the split ratio — what
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

export type PaneSide = 'left' | 'right'

export function pageTabId(path: string): string {
  return `page:${path}`
}

export function tabEntryKey(e: TabEntry): string {
  return e.type === 'project' ? `project:${e.projectId}` : e.id
}

/**
 * A project tab remembering a run of another project (pinned before
 * cross-project workers were filed correctly) resumes on its project instead:
 * it would otherwise open that other project's session under this tab.
 * `ownerOf` gives a run's project, when known.
 */
export function withOwnRun<T extends TabEntry>(e: T, ownerOf: (runId: string) => string | undefined): T {
  if (e.type !== 'project' || !e.lastRunId) return e
  const owner = ownerOf(e.lastRunId)
  return owner && owner !== e.projectId ? { ...e, lastRunId: null } : e
}

// --- Split ratio --------------------------------------------------------------

export const DEFAULT_SPLIT_RATIO = 0.5
export const SPLIT_MIN_LEFT = 360
export const SPLIT_MIN_RIGHT = 320

/**
 * A loaded / persisted share (0–1) of the left pane: anything that isn't a
 * share strictly between 0 and 1 falls back to 50/50. Values clamped on a wide
 * window (`clampRatio`, e.g. 0.97) must survive; the panes' CSS min widths keep
 * them usable on a narrower one.
 */
export function sanitizeRatio(v: unknown): number {
  return typeof v === 'number' && Number.isFinite(v) && v > 0 && v < 1 ? v : DEFAULT_SPLIT_RATIO
}

/**
 * The left pane's share of `total` px once both panes keep their minimum
 * width. When both minimums don't fit, they shrink proportionally. Any finite
 * input is clamped (a drag past an edge, Home / End), never reset to 50/50.
 */
export function clampRatio(
  ratio: number,
  total: number,
  minLeft = SPLIT_MIN_LEFT,
  minRight = SPLIT_MIN_RIGHT,
): number {
  if (!Number.isFinite(ratio)) return DEFAULT_SPLIT_RATIO
  const r = Math.min(1, Math.max(0, ratio))
  if (!(total > 0)) return sanitizeRatio(r)
  if (minLeft + minRight >= total) return minLeft / (minLeft + minRight)
  return Math.min(1 - minRight / total, Math.max(minLeft / total, r))
}

// --- Persistence ----------------------------------------------------------------

export const WORKSPACE_STATE_VERSION = 3

export interface WorkspaceState {
  entries: TabEntry[]
  /** Split view on: the session on the left, any other screen on the right. */
  split: boolean
  /** The session (project tab key) the left half shows — the last one viewed. */
  left: string | null
  /** The tab (never a session) in the right half, or null for an empty half. */
  right: string | null
  ratio: number
}

/** Which half a tab shows in while split: sessions on the left, everything else on the right. */
export function splitSide(e: TabEntry): PaneSide {
  return e.type === 'project' ? 'left' : 'right'
}

/**
 * The session for the left half: `preferred` while it is an open session tab,
 * else the first session tab on the strip, else null (no session to show).
 */
export function pickSession(entries: readonly TabEntry[], preferred: string | null | undefined): string | null {
  if (preferred && entries.some((e) => e.type === 'project' && tabEntryKey(e) === preferred)) return preferred
  const first = entries.find((e) => e.type === 'project')
  return first ? tabEntryKey(first) : null
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
 * was never saved); version 2 kept a free pair of tabs (`pair`, `pairShown`),
 * read as the split when one half is a session; version 3 is
 * `{ version, entries, split, left, right, ratio }`. Anything malformed is
 * dropped rather than failing the whole strip.
 */
export function parseWorkspaceState(raw: string | null | undefined): WorkspaceState {
  const empty: WorkspaceState = { entries: [], split: false, left: null, right: null, ratio: DEFAULT_SPLIT_RATIO }
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
  const byKey = (k: unknown) => (typeof k === 'string' ? (entries.find((e) => tabEntryKey(e) === k) ?? null) : null)
  let left: unknown = obj.left
  let right: unknown = obj.right
  let split = obj.split === true
  // v2: a pair with a session half becomes the split; any other pair is dropped.
  const p = obj.pair as Record<string, unknown> | null | undefined
  if (obj.version === 2 && p) {
    const halves = [byKey(p.left), byKey(p.right)]
    const session = halves.find((e) => e?.type === 'project')
    const other = halves.find((e) => e && e.type !== 'project')
    left = session ? tabEntryKey(session) : null
    right = session && other ? tabEntryKey(other) : null
    split = !!session && obj.pairShown === true
  }
  const l = byKey(left)
  const r = byKey(right)
  const leftKey = l && splitSide(l) === 'left' ? tabEntryKey(l) : null
  return {
    entries,
    left: leftKey,
    right: r && splitSide(r) === 'right' ? tabEntryKey(r) : null,
    // Never split without a session to show on the left.
    split: split && !!pickSession(entries, leftKey),
    ratio,
  }
}

export function serializeWorkspaceState(s: WorkspaceState): string {
  return JSON.stringify({
    version: WORKSPACE_STATE_VERSION,
    entries: s.entries,
    split: s.split === true,
    left: s.left,
    right: s.right,
    ratio: s.ratio,
  })
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
