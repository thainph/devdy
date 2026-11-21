// THE way to open a surface (PR Inbox, Calendar, Slack threads, Todos, a
// file…) from anywhere: it decides tab vs window (lib/surfacePlacement) and
// either routes the main window to the surface's tab or opens its OS window.
//
// From a pop-out webview (no router of the main window) a tab request is sent
// to the main window over SURFACE_OPEN, handled by `listenSurfaceRequests`.
import { emit, listen, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow'
import router from '@/router'
import { useWorkspaceTabsStore, type TabEntry } from '@/stores/workspaceTabs'
import { useGoogleCalendarStore } from '@/stores/googleCalendar'
import {
  SLACK_WINDOW_ADD,
  SLACK_WINDOW_SELECT,
  type SlackWindowAdd,
  type SlackWindowSelect,
} from '@/lib/slackWindow'
import { itemListSelectEvent, type ItemListSelect } from '@/lib/itemListWindow'
import { SURFACE_DOCK, SURFACE_DOCK_ALL, type SurfaceDockRequest } from '@/lib/surfaceDock'
import {
  cleanParams,
  isSurfaceKind,
  surfaceForPath,
  surfaceLocation,
  surfaceTabId,
  type Placement,
  type SurfaceKind,
  type SurfaceParams,
} from '@/lib/surfaces'
import {
  isMainWindow,
  openSurfaceWindow,
  resolvePlacement,
  surfaceWindowOpen,
  type SurfaceExtras,
} from '@/lib/surfacePlacement'

/** Pop-out → main window: "show this surface as a tab". */
export const SURFACE_OPEN = 'surface:open'

export interface SurfaceOpenRequest {
  kind: SurfaceKind
  params?: SurfaceParams
  extras?: SurfaceExtras
}

export interface OpenSurfaceOptions extends SurfaceExtras {
  /** Skip the settings and open as this. */
  placement?: Placement
  /**
   * As a tab, show it in the right half of a split instead of switching the
   * main area to it — e.g. a file opened from a session keeps the session in
   * view. Main window only.
   */
  split?: boolean
}

async function focusMain() {
  try {
    const win = getCurrentWebviewWindow()
    await win.unminimize()
    await win.show()
    await win.setFocus()
  } catch {
    /* best-effort */
  }
}

/** Open `kind` as a tab of the main window (must run in the main window). */
async function openAsTab(kind: SurfaceKind, params: SurfaceParams, extras: SurfaceExtras, split = false) {
  const tabs = useWorkspaceTabsStore()
  const tab = tabs.openSurface(kind, params)
  const routedHere = surfaceTabIdOfRoute() === tab.id
  if (split && !routedHere) {
    tabs.split(tab.id)
  } else if (tabs.splitId === tab.id) {
    // Already in the right half: just bring that half forward.
    tabs.focusedPane = 'right'
  } else {
    await router.push(surfaceLocation(kind, tab.params, true)).catch(() => {})
  }
  // Re-select even when the tab already pointed there (the user may have
  // moved on inside it): params alone wouldn't change, the views hear these.
  if (params.id && kind === 'slack') {
    void emit(SLACK_WINDOW_SELECT, { id: params.id } satisfies SlackWindowSelect).catch(() => {})
  } else if (params.id && (kind === 'todos' || kind === 'notes')) {
    void emit(itemListSelectEvent(kind === 'todos' ? 'todo' : 'note'), { id: params.id } satisfies ItemListSelect).catch(
      () => {},
    )
  }
  if (extras.calendarEvent) useGoogleCalendarStore().requestOpenEvent(extras.calendarEvent)
  if (extras.slackAdd) {
    void emit(SLACK_WINDOW_ADD, { projectId: params.projectId ?? null } satisfies SlackWindowAdd).catch(() => {})
  }
  await focusMain()
}

/** Open a surface where the user wants it (tab or window). */
export async function openSurface(
  kind: SurfaceKind,
  rawParams: Record<string, unknown> = {},
  opts: OpenSurfaceOptions = {},
): Promise<void> {
  const params = cleanParams(kind, rawParams)
  const extras: SurfaceExtras = { calendarEvent: opts.calendarEvent, slackAdd: opts.slackAdd }
  // Where it already is wins over where it would open: never a tab beside its
  // own window, or a window beside its own tab.
  let where = opts.placement
  if (!where && (await surfaceWindowOpen(kind, params))) where = 'window'
  if (!where && isMainWindow() && useWorkspaceTabsStore().surfaceTab(surfaceTabId(kind, params))) where = 'tab'
  where ??= await resolvePlacement(kind)
  if (where === 'window') {
    await openSurfaceWindow(kind, params, extras)
    return
  }
  if (isMainWindow()) {
    await openAsTab(kind, params, extras, opts.split)
  } else if (opts.split) {
    // "Beside what I'm looking at" from a pop-out (a session window): the main
    // window is not beside it, so open its own window as before.
    await openSurfaceWindow(kind, params, extras)
  } else {
    await emit(SURFACE_OPEN, { kind, params, extras } satisfies SurfaceOpenRequest).catch(() => {})
  }
}

/** Move a surface tab out to its own window (the tab closes). */
export async function popOutSurface(id: string): Promise<void> {
  const tabs = useWorkspaceTabsStore()
  const tab = tabs.surfaceTab(id)
  if (!tab) return
  const onIt = surfaceTabIdOfRoute() === id
  const next = tabs.closeSurface(id)
  if (onIt) await goToEntry(skipSplit(next))
  await openSurfaceWindow(tab.surface, tab.params)
}

/**
 * Close a surface tab from code (its view asked: a deleted file…), moving the
 * router on if the tab was what it showed.
 */
export async function closeSurfaceTab(id: string): Promise<void> {
  const tabs = useWorkspaceTabsStore()
  const tab = tabs.surfaceTab(id)
  if (!tab) return
  const routed = surfaceTabIdOfRoute() === id
  const next = tabs.closeSurface(id)
  if (routed) await goToEntry(skipSplit(next))
}

/** The split's right pane is not a destination of the router: skip over it. */
export function skipSplit(next: TabEntry | null): TabEntry | null {
  return next && next.type !== 'project' && next.id === useWorkspaceTabsStore().splitId ? null : next
}

/** The surface tab the router shows right now, if any. */
export function surfaceTabIdOfRoute(): string | null {
  const route = router.currentRoute.value
  const kind = surfaceForPath(route.path)
  return kind ? surfaceTabId(kind, cleanParams(kind, route.query)) : null
}

/** Navigate to a tab entry (after closing the current one), or the projects list. */
export async function goToEntry(next: TabEntry | null) {
  if (!next) {
    await router.push({ name: 'dashboard' }).catch(() => {})
    return
  }
  if (next.type === 'surface') {
    await router.push(surfaceLocation(next.surface, next.params, true)).catch(() => {})
    return
  }
  if (next.type === 'page') {
    await router.push(next.fullPath).catch(() => {})
    return
  }
  if (next.lastRunId) {
    await router
      .push({ name: 'project-run-detail', params: { projectId: next.projectId, runId: next.lastRunId } })
      .catch(() => {})
  } else {
    await router.push({ name: 'project-run', params: { projectId: next.projectId } }).catch(() => {})
  }
}

// Docks received while `dockAllSurfaceWindows` is collecting them.
let dockCounter: { n: number } | null = null

/**
 * Ask every surface pop-out to dock itself back into the main window. Resolves
 * with how many did, once they had a moment to answer.
 */
export async function dockAllSurfaceWindows(): Promise<number> {
  const counter = { n: 0 }
  dockCounter = counter
  await emit(SURFACE_DOCK_ALL).catch(() => {})
  await new Promise((r) => setTimeout(r, 1500))
  if (dockCounter === counter) dockCounter = null
  return counter.n
}

/**
 * Main window: serve tab requests from pop-outs (SURFACE_OPEN) and windows
 * docking themselves back (SURFACE_DOCK). Returns the unlisten.
 */
export async function listenSurfaceRequests(): Promise<UnlistenFn> {
  const offs: UnlistenFn[] = []
  try {
    offs.push(
      await listen<SurfaceOpenRequest>(SURFACE_OPEN, (e) => {
        const p = e.payload
        if (p && isSurfaceKind(p.kind)) void openAsTab(p.kind, cleanParams(p.kind, p.params), p.extras ?? {})
      }),
    )
    offs.push(
      await listen<SurfaceDockRequest>(SURFACE_DOCK, (e) => {
        const p = e.payload
        if (!p || !isSurfaceKind(p.surface)) return
        if (dockCounter) dockCounter.n++
        void openAsTab(p.surface, cleanParams(p.surface, p.params), {})
      }),
    )
  } catch {
    /* outside the Tauri shell */
  }
  return () => offs.forEach((off) => off())
}
