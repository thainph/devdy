// "Surfaces": the screens that can live either as a tab inside the main window
// or in their own OS window (PR Inbox, GitHub Mentions, Calendar, Delivery
// Radar, Slack threads, Todos, Notes, file viewers).
//
// Pure logic only — no Tauri / router imports — so it is unit-tested directly
// (src/lib/__tests__/surfaces.test.mjs). Opening lives in lib/surfaceOpen.ts.

export type SurfaceKind =
  | 'prInbox'
  | 'mentions'
  | 'calendar'
  | 'radar'
  | 'slack'
  | 'todos'
  | 'notes'
  | 'file'

export const SURFACE_KINDS: readonly SurfaceKind[] = [
  'prInbox',
  'mentions',
  'calendar',
  'radar',
  'slack',
  'todos',
  'notes',
  'file',
]

/** Global setting `window_mode`. */
export type WindowMode = 'auto' | 'tab' | 'window'
export type Placement = 'tab' | 'window'

/** Route path of each surface when it is shown as a tab of the main window. */
export const SURFACE_PATHS: Record<SurfaceKind, string> = {
  prInbox: '/pr-inbox',
  mentions: '/github-mentions',
  calendar: '/calendar',
  radar: '/radar',
  slack: '/slack',
  todos: '/todos',
  notes: '/notes',
  file: '/file',
}

/**
 * Surfaces that "Auto" moves to their own window once a second monitor is
 * attached — the dashboards worth keeping in view beside the work. Todos,
 * Notes and files stay tabs (the user can still pop any tab out).
 */
export const AUTO_WINDOW_SURFACES: ReadonlySet<SurfaceKind> = new Set([
  'prInbox',
  'mentions',
  'calendar',
  'radar',
  'slack',
])

/** Query keys each surface keeps in its route / tab (everything else is dropped). */
const PARAM_KEYS: Record<SurfaceKind, readonly string[]> = {
  prInbox: [],
  mentions: [],
  calendar: [],
  radar: ['project'],
  slack: ['id', 'projectId'],
  todos: ['id', 'projectId'],
  notes: ['id', 'projectId'],
  file: ['projectPath', 'path', 'line'],
}

export type SurfaceParams = Record<string, string>

export function isSurfaceKind(v: unknown): v is SurfaceKind {
  return typeof v === 'string' && (SURFACE_KINDS as readonly string[]).includes(v)
}

export function parseWindowMode(v: string | null | undefined): WindowMode {
  return v === 'tab' || v === 'window' ? v : 'auto'
}

/** Parse the `window_mode_overrides` JSON, dropping anything malformed. */
export function parseOverrides(raw: string | null | undefined): Partial<Record<SurfaceKind, Placement>> {
  const out: Partial<Record<SurfaceKind, Placement>> = {}
  if (!raw) return out
  let obj: unknown
  try {
    obj = JSON.parse(raw)
  } catch {
    return out
  }
  if (!obj || typeof obj !== 'object' || Array.isArray(obj)) return out
  for (const [k, v] of Object.entries(obj as Record<string, unknown>)) {
    if (isSurfaceKind(k) && (v === 'tab' || v === 'window')) out[k] = v
  }
  return out
}

/**
 * Where `kind` opens: its own override first, then the global mode; "auto"
 * means a tab on one monitor and — for the dashboard-like surfaces — a window
 * once there are two or more.
 */
export function placementFor(
  kind: SurfaceKind,
  mode: WindowMode,
  overrides: Partial<Record<SurfaceKind, Placement>>,
  monitorCount: number,
): Placement {
  const own = overrides[kind]
  if (own) return own
  if (mode === 'tab' || mode === 'window') return mode
  return monitorCount >= 2 && AUTO_WINDOW_SURFACES.has(kind) ? 'window' : 'tab'
}

/** Keep only the params a surface understands, as non-empty strings. */
export function cleanParams(kind: SurfaceKind, params: Record<string, unknown> | null | undefined): SurfaceParams {
  const out: SurfaceParams = {}
  if (!params) return out
  for (const key of PARAM_KEYS[kind]) {
    const v = params[key]
    if (typeof v === 'string' && v !== '') out[key] = v
    else if (typeof v === 'number' && Number.isFinite(v)) out[key] = String(v)
  }
  return out
}

/**
 * Identity of a surface tab: one tab per surface, except files (one per file).
 * Opening Todos on another item re-points the existing Todos tab.
 */
export function surfaceTabId(kind: SurfaceKind, params: SurfaceParams = {}): string {
  if (kind === 'file') return `surface:file:${params.projectPath ?? ''}|${params.path ?? ''}`
  return `surface:${kind}`
}

/** The surface a route path shows, or null for any other screen. */
export function surfaceForPath(path: string): SurfaceKind | null {
  for (const kind of SURFACE_KINDS) {
    if (path === SURFACE_PATHS[kind]) return kind
  }
  return null
}

/** Router location of a surface tab. */
export function surfaceLocation(kind: SurfaceKind, params: SurfaceParams = {}): { path: string; query: SurfaceParams } {
  return { path: SURFACE_PATHS[kind], query: cleanParams(kind, params) }
}

/**
 * Which surfaces' windows should come back as tabs after the monitor count
 * dropped from `before` to `after`. Only a drop to a single monitor docks —
 * three-to-two keeps everything where macOS moves it.
 */
export function shouldDockOnMonitorChange(before: number, after: number, dockOnUnplug: boolean): boolean {
  return dockOnUnplug && before >= 2 && after <= 1
}
