// THE way to open a surface (PR Inbox, Calendar, Slack threads, Todos, a
// file…) from anywhere: as a tab of the main window, unless the caller asks
// for a window or the surface already has one open (then that one is focused).
//
// From a pop-out webview (no router of the main window) a tab request is sent
// to the main window over SURFACE_OPEN, handled by `listenSurfaceRequests`.
import { emit, listen, type UnlistenFn } from '@tauri-apps/api/event'
import type { RouteLocationRaw } from 'vue-router'
import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow'
import { ref } from 'vue'
import router from '@/router'
import { tabEntryKey, useWorkspaceTabsStore, type TabEntry } from '@/stores/workspaceTabs'
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
  surfaceWindowOpen,
  type SurfaceExtras,
} from '@/lib/surfacePlacement'
import { entryTargetFor, entryTargetKey, type EntryTarget } from '@/lib/workspaceState'
import { pageRouteFor } from '@/lib/navigation'

/** Pop-out → main window: "show this surface as a tab". */
export const SURFACE_OPEN = 'surface:open'

export interface SurfaceOpenRequest {
  kind: SurfaceKind
  params?: SurfaceParams
  extras?: SurfaceExtras
}

export interface OpenSurfaceOptions extends SurfaceExtras {
  /** Open as this (default: a tab, or the surface's window when one is open). */
  placement?: Placement
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

/**
 * Open `kind` as a tab of the main window (must run in the main window). Full
 * width, the main area switches to it; split, the navigation guard shows it in
 * the right half instead (lib/tabActions `routeSplitNavigation`).
 */
async function openAsTab(kind: SurfaceKind, params: SurfaceParams, extras: SurfaceExtras) {
  const tabs = useWorkspaceTabsStore()
  const tab = tabs.openSurface(kind, params)
  await router.push(surfaceLocation(kind, tab.params, true)).catch(() => {})
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

/** Open a surface where the user wants it; resolves to where it opened (tab or window). */
export async function openSurface(
  kind: SurfaceKind,
  rawParams: Record<string, unknown> = {},
  opts: OpenSurfaceOptions = {},
): Promise<Placement> {
  const params = cleanParams(kind, rawParams)
  const extras: SurfaceExtras = { calendarEvent: opts.calendarEvent, slackAdd: opts.slackAdd }
  // Where it already is wins over where it would open: never a tab beside its
  // own window, or a window beside its own tab.
  let where = opts.placement
  if (!where && (await surfaceWindowOpen(kind, params))) where = 'window'
  where ??= 'tab'
  if (where === 'window') {
    await openSurfaceWindow(kind, params, extras)
    return 'window'
  }
  if (isMainWindow()) {
    await openAsTab(kind, params, extras)
  } else {
    await emit(SURFACE_OPEN, { kind, params, extras } satisfies SurfaceOpenRequest).catch(() => {})
  }
  return 'tab'
}

/** Move a surface tab out to its own window (the tab closes). */
export async function popOutSurface(id: string): Promise<void> {
  const tab = useWorkspaceTabsStore().entry(id)
  if (tab?.type !== 'surface') return
  await removeTab(tab)
  await openSurfaceWindow(tab.surface, tab.params)
}

/**
 * Close a surface tab from code (its view asked: a deleted file…), moving the
 * router on if the tab was what it showed.
 */
export async function closeSurfaceTab(id: string): Promise<void> {
  const tab = useWorkspaceTabsStore().entry(id)
  if (tab?.type === 'surface') await removeTab(tab)
}

/**
 * Close a tab and move the view on. Full width, closing the tab on screen goes
 * to its neighbour. Split, closing the right half's tab leaves that half empty;
 * closing the session on the left brings another session there, and with none
 * left the split ends, the right half's screen showing full width.
 */
export async function removeTab(e: TabEntry): Promise<void> {
  const tabs = useWorkspaceTabsStore()
  const key = tabEntryKey(e)
  const wasRouted = key === routedKey()
  const wasRight = tabs.isSplit && key === tabs.splitId
  const right = tabs.entry(tabs.splitId)
  const next =
    e.type === 'project' ? tabs.close(e.projectId) : e.type === 'page' ? tabs.closePage(e.id) : tabs.closeSurface(e.id)
  tabs.focusedPane = 'left'
  if (!tabs.isSplit) {
    if (wasRouted) await goToEntry(next)
    return
  }
  if (wasRight || !wasRouted) return
  const session = tabs.entry(tabs.sessionForLeft())
  if (session) {
    await goToEntry(session)
    return
  }
  tabs.setSplitMode(false)
  await goToEntry(right ?? next)
}

/** The surface tab the router shows right now, if any. */
export function surfaceTabIdOfRoute(): string | null {
  const route = router.currentRoute.value
  const kind = surfaceForPath(route.path)
  return kind ? surfaceTabId(kind, cleanParams(kind, route.query)) : null
}

/** The tab the router shows (the left / only pane), as data; null for a non-tab screen. */
export function routedTarget(): EntryTarget | null {
  return entryTargetFor(router.currentRoute.value, surfaceForPath, (p) => !!pageRouteFor(p))
}

/** Key of the tab the router shows, or null for a screen that isn't a tab. */
export function routedTargetKey(): string | null {
  const t = routedTarget()
  return t ? entryTargetKey(t) : null
}

/**
 * Key of the tab the router's screen belongs to, for highlighting: like
 * `routedTargetKey`, plus a project's other screens (its settings) count as
 * its tab.
 */
export function routedKey(): string | null {
  const key = routedTargetKey()
  if (key) return key
  const id = router.currentRoute.value.params.projectId
  return typeof id === 'string' ? `project:${id}` : null
}

/** Router location of a tab (where clicking it goes). */
export function entryLocation(e: TabEntry): RouteLocationRaw {
  if (e.type === 'surface') return surfaceLocation(e.surface, e.params, true)
  if (e.type === 'page') return e.fullPath
  return e.lastRunId
    ? { name: 'project-run-detail', params: { projectId: e.projectId, runId: e.lastRunId } }
    : { name: 'project-run', params: { projectId: e.projectId } }
}

/**
 * Where the right half of the split is, when it isn't simply its tab's own
 * location: a page tab moved on inside itself (keyed by that tab, so it only
 * applies to it), or a screen that isn't a tab at all — a project's settings —
 * with `key` null (shown while the right half has no tab).
 */
export const rightPaneLocation = ref<{ key: string | null; fullPath: string } | null>(null)

/** Navigate to a tab entry (after closing the current one), or the dashboard. */
export async function goToEntry(next: TabEntry | null) {
  await router.push(next ? entryLocation(next) : { name: 'dashboard' }).catch(() => {})
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
