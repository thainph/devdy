// Actions on the main window's tab strip (stores/workspaceTabs), shared by the
// strip itself (components/WorkspaceTabs.vue), the command palette and the
// tab shortcuts of the native menu (⌘1…⌘9, close tab, split). Main window only:
// they drive its router.
//
// Split view is a mode of the one main area, on or off. Off (full), every
// screen shows full width. On, the left half always shows a session and the
// right half any other screen: the router shows the session, and a navigation
// anywhere else — a tab, the sidebar, a link, openSurface — is caught by
// `routeSplitNavigation` and shown in the right half instead (App.vue).
import { nextTick } from 'vue'
import type { RouteLocationNormalizedLoaded } from 'vue-router'
import router from '@/router'
import { tabEntryKey, useWorkspaceTabsStore, type TabEntry } from '@/stores/workspaceTabs'
import { useRunsStore } from '@/stores/runs'
import { surfaceForPath } from '@/lib/surfaces'
import { pageRouteFor } from '@/lib/navigation'
import { entryTargetFor, withOwnRun, type EntryTarget } from '@/lib/workspaceState'
import {
  entryLocation,
  goToEntry,
  popOutSurface,
  removeTab,
  rightPaneLocation,
  routedKey,
  routedTargetKey,
} from '@/lib/surfaceOpen'

/** Shown in either half of the main area. */
export function isEntryVisible(e: TabEntry): boolean {
  const key = tabEntryKey(e)
  return key === routedKey() || key === useWorkspaceTabsStore().splitId
}

/** The tab the user is on: the focused half's tab, if it has one. */
export function currentEntry(): TabEntry | null {
  const tabs = useWorkspaceTabsStore()
  if (tabs.focusedPane === 'right' && tabs.isSplit) return tabs.entry(tabs.splitId)
  return tabs.entry(routedKey())
}

/** A project tab remembering a run of another project resumes on the project instead. */
export function sanitized(e: TabEntry): TabEntry {
  const runs = useRunsStore()
  return withOwnRun(e, (runId) => runs.runMeta.get(runId)?.project_id)
}

/**
 * Startup: the router's first navigation (`/` → `/dashboard`) is not the user
 * choosing a screen, so while split it must not land in the right half. Until
 * it has landed, navigations pass untouched; then a restored split routes to
 * its session.
 */
let startupSettled = false

/** Main window, once: lift the startup gate (see above). */
export async function settleStartupNavigation() {
  await router.isReady()
  // Let the route watcher handle the first navigation while still gated.
  await nextTick()
  startupSettled = true
  const tabs = useWorkspaceTabsStore()
  if (!tabs.isSplit) return
  if (!routeToLeft()) tabs.setSplitMode(false)
}

/** Make the router show the left half's session. False when there is none. */
function routeToLeft(): boolean {
  const tabs = useWorkspaceTabsStore()
  const key = tabs.sessionForLeft()
  const left = tabs.entry(key)
  if (!left) return false
  if (routedTargetKey() !== key) void goToEntry(sanitized(left))
  return true
}

/** Go to a tab, as a click on it in the strip does (split: it lands in its half). */
export function selectEntry(e: TabEntry) {
  const tabs = useWorkspaceTabsStore()
  const key = tabEntryKey(e)
  if (tabs.isSplit && key === tabs.splitId) {
    tabs.focusedPane = 'right'
    return
  }
  if (key === routedKey()) {
    tabs.focusedPane = 'left'
    return
  }
  void goToEntry(sanitized(e))
}

/** Close a tab; when it was on screen, move to its neighbour (or another session). */
export function closeEntry(e: TabEntry) {
  void removeTab(e)
}

/**
 * Close every tab but `e`, which is left on screen full width (split ends).
 * The others close straight in the store, so the router moves once, to `e`,
 * instead of hopping through each neighbour.
 */
export async function closeOtherEntries(e: TabEntry) {
  const tabs = useWorkspaceTabsStore()
  const keep = tabEntryKey(e)
  tabs.setSplitMode(false)
  if (keep !== routedKey()) await goToEntry(sanitized(e))
  for (const other of [...tabs.entries]) {
    if (tabEntryKey(other) === keep) continue
    if (other.type === 'project') tabs.close(other.projectId)
    else if (other.type === 'page') tabs.closePage(other.id)
    else tabs.closeSurface(other.id)
  }
}

/** Whether split view can be turned on: it needs a session for the left half. */
export function canSplit(): boolean {
  const tabs = useWorkspaceTabsStore()
  return tabs.isSplit || !!tabs.sessionForLeft()
}

/**
 * ⌘\ / the strip's button — split view on or off.
 *
 * On: the session on screen (else the last one viewed) goes left; the screen
 * on screen, when it isn't a session, goes right (else the right half keeps
 * what it had last). Off: the focused half shows full width.
 */
export function toggleSplit() {
  const tabs = useWorkspaceTabsStore()
  if (tabs.isSplit) {
    const right = tabs.focusedPane === 'right' ? rightLocation() : null
    tabs.setSplitMode(false)
    if (right) void router.push(right).catch(() => {})
    return
  }
  if (!tabs.sessionForLeft()) return
  const routedEntry = tabs.entry(routedTargetKey())
  const route = router.currentRoute.value
  if (routedEntry?.type === 'project') {
    tabs.setLeft(tabEntryKey(routedEntry))
  } else if (routedEntry) {
    tabs.setRight(tabEntryKey(routedEntry))
    if (routedEntry.type === 'page') rightPaneLocation.value = { key: tabEntryKey(routedEntry), fullPath: route.fullPath }
  } else if (route.name) {
    // A screen that isn't a tab (a project's settings).
    tabs.setRight(null)
    rightPaneLocation.value = { key: null, fullPath: route.fullPath }
  }
  tabs.setSplitMode(true)
  tabs.focusedPane = routedEntry?.type === 'project' ? 'left' : 'right'
  routeToLeft()
}

/** Where the right half is, as a path (to show it full width), or null when empty. */
function rightLocation(): string | null {
  const tabs = useWorkspaceTabsStore()
  const moved = rightPaneLocation.value
  const right = tabs.entry(tabs.splitId)
  if (right) {
    if (moved && moved.key === tabEntryKey(right)) return moved.fullPath
    return router.resolve(entryLocation(right)).fullPath
  }
  return moved && moved.key === null ? moved.fullPath : null
}

/** ⌘1…⌘8 go to that tab; ⌘9 (n = 9) to the last one, as browsers do. */
export function goToTabNumber(n: number) {
  const entries = useWorkspaceTabsStore().entries
  const e = n >= 9 ? entries[entries.length - 1] : entries[n - 1]
  if (e) selectEntry(e)
}

/** ⌘⇧[ / ⌘⇧]: the tab before / after the current one, wrapping around. */
export function goToTabOffset(delta: number) {
  const entries = useWorkspaceTabsStore().entries
  if (!entries.length) return
  const cur = currentEntry()
  const at = cur ? entries.findIndex((e) => tabEntryKey(e) === tabEntryKey(cur)) : -1
  const next = at < 0 ? (delta > 0 ? 0 : entries.length - 1) : (at + delta + entries.length) % entries.length
  selectEntry(entries[next])
}

export function closeCurrentTab() {
  const cur = currentEntry()
  if (cur) closeEntry(cur)
}

/** Move the current tab to its own window (surfaces only). */
export function popOutCurrent() {
  const cur = currentEntry()
  if (cur?.type === 'surface') void popOutSurface(cur.id)
}

// --- Split: which half a navigation lands in ---------------------------------

function targetOf(to: RouteLocationNormalizedLoaded): EntryTarget | null {
  return entryTargetFor(to, surfaceForPath, (p) => !!pageRouteFor(p))
}

/**
 * Global navigation guard (main window). While split, the router only ever
 * shows the session on the left: a navigation to a session passes (the left
 * half follows it); anything else — a surface, a page, a screen that isn't a
 * tab — is shown in the right half and the router's navigation is cancelled
 * (returns false).
 */
export function routeSplitNavigation(to: RouteLocationNormalizedLoaded): boolean {
  if (!startupSettled) return true
  const tabs = useWorkspaceTabsStore()
  if (!tabs.isSplit) return true
  const target = targetOf(to)
  if (target?.type === 'project') {
    tabs.focusedPane = 'left'
    return true
  }
  if (!target) {
    tabs.setRight(null)
    rightPaneLocation.value = { key: null, fullPath: to.fullPath }
  } else if (target.type === 'page') {
    const key = tabs.openPage(target.path, target.fullPath).id
    tabs.setRight(key)
    rightPaneLocation.value = { key, fullPath: to.fullPath }
  } else {
    tabs.setRight(tabs.openSurface(target.surface, target.params).id)
  }
  tabs.focusedPane = 'right'
  return false
}

/**
 * A screen in the right half navigated (PaneRouterView's push / replace): the
 * router takes it, and `routeSplitNavigation` decides — a session goes left,
 * anything else stays in the right half.
 */
export function navigateRightPane(to: RouteLocationNormalizedLoaded, replace = false) {
  const go = replace ? router.replace(to.fullPath) : router.push(to.fullPath)
  void go.catch(() => {})
}

/** The sidebar's links: the router takes them (split: into their half). */
export function navigateFocused(path: string) {
  void router.push(path).catch(() => {})
}
