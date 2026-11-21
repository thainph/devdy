// Actions on the main window's tab strip (stores/workspaceTabs), shared by the
// strip itself (components/WorkspaceTabs.vue), the command palette and the
// tab shortcuts of the native menu (⌘1…⌘9, close tab, split). Main window only:
// they drive its router.
import router from '@/router'
import { tabEntryKey, useWorkspaceTabsStore, type TabEntry } from '@/stores/workspaceTabs'
import { useRunsStore } from '@/stores/runs'
import { cleanParams, surfaceForPath, surfaceTabId } from '@/lib/surfaces'
import { goToEntry, popOutSurface } from '@/lib/surfaceOpen'

type SurfaceEntry = Extract<TabEntry, { type: 'surface' }>

function activeProjectId(): string | null {
  const id = router.currentRoute.value.params.projectId
  return typeof id === 'string' ? id : null
}

/** The surface tab the router is showing (the left / only pane), if any. */
export function routedSurfaceId(): string | null {
  const route = router.currentRoute.value
  const kind = surfaceForPath(route.path)
  return kind ? surfaceTabId(kind, cleanParams(kind, route.query)) : null
}

/** Shown in either half of the main area. */
export function isEntryVisible(e: TabEntry): boolean {
  if (e.type === 'project') return e.projectId === activeProjectId()
  if (e.type === 'page') return e.path === router.currentRoute.value.path
  return e.id === useWorkspaceTabsStore().splitId || e.id === routedSurfaceId()
}

/** The tab the user is on: the focused half's tab, if it has one. */
export function currentEntry(): TabEntry | null {
  const tabs = useWorkspaceTabsStore()
  if (tabs.focusedPane === 'right' && tabs.splitId) {
    return tabs.entries.find((e) => tabEntryKey(e) === tabs.splitId) ?? null
  }
  const surfaceId = routedSurfaceId()
  const projectId = activeProjectId()
  const path = router.currentRoute.value.path
  return (
    tabs.entries.find((e) =>
      e.type === 'surface' ? e.id === surfaceId : e.type === 'page' ? e.path === path : e.projectId === projectId,
    ) ?? null
  )
}

/** Go to a tab, as a click on it in the strip does. */
export function selectEntry(e: TabEntry) {
  const tabs = useWorkspaceTabsStore()
  if (e.type === 'surface' && e.id === tabs.splitId) {
    tabs.focusedPane = 'right'
    return
  }
  tabs.focusedPane = 'left'
  if (isEntryVisible(e)) return
  if (e.type === 'project') {
    // A remembered run from another project (pinned before cross-project workers
    // were filed correctly) would open that project's session under this tab.
    const lastOwner = e.lastRunId ? useRunsStore().runMeta.get(e.lastRunId)?.project_id : undefined
    void goToEntry(e.lastRunId && lastOwner && lastOwner !== e.projectId ? { ...e, lastRunId: null } : e)
    return
  }
  void goToEntry(e)
}

/** Close a tab; when the router was showing it, move to its neighbour. */
export function closeEntry(e: TabEntry) {
  const tabs = useWorkspaceTabsStore()
  const wasRouted =
    e.type === 'project'
      ? e.projectId === activeProjectId()
      : e.type === 'page'
        ? e.path === router.currentRoute.value.path
        : e.id === routedSurfaceId()
  const next =
    e.type === 'project' ? tabs.close(e.projectId) : e.type === 'page' ? tabs.closePage(e.id) : tabs.closeSurface(e.id)
  if (!wasRouted) return
  // The split's right pane is not a destination of the router: skip over it.
  const target = next && next.type === 'surface' && next.id === tabs.splitId ? null : next
  tabs.focusedPane = 'left'
  void goToEntry(target)
}

/** Show a surface tab in the right half, or close the right half if it's there. */
export function toggleSplitEntry(e: SurfaceEntry) {
  const tabs = useWorkspaceTabsStore()
  if (tabs.splitId === e.id) {
    tabs.unsplit()
    return
  }
  // Moving the routed surface to the right leaves the left on another tab.
  const wasRouted = e.id === routedSurfaceId()
  tabs.split(e.id)
  if (wasRouted) {
    const other = tabs.entries.find((x) => tabEntryKey(x) !== e.id)
    void goToEntry(other ?? null)
  }
}

/**
 * The surface tab "split / unsplit" acts on: the current tab when it's a
 * surface, else (on a session) the right-most surface tab, to show beside it.
 * Null when there's nothing to split — or when already split (then it unsplits).
 */
export function splitCandidate(): SurfaceEntry | null {
  const tabs = useWorkspaceTabsStore()
  const cur = currentEntry()
  if (cur?.type === 'surface') return cur
  const surfaces = tabs.entries.filter((e): e is SurfaceEntry => e.type === 'surface')
  return surfaces[surfaces.length - 1] ?? null
}

/** ⌘\ — close the right half, or open it with `splitCandidate()`. */
export function toggleSplitCurrent() {
  const tabs = useWorkspaceTabsStore()
  if (tabs.splitId) {
    tabs.unsplit()
    return
  }
  const target = splitCandidate()
  // Splitting needs something left over for the left half.
  if (target && tabs.entries.length > 1) toggleSplitEntry(target)
}

/** ⌘1…⌘8 go to that tab; ⌘9 (n = 9) to the last one, as browsers do. */
export function goToTabNumber(n: number) {
  const entries = useWorkspaceTabsStore().entries
  const e = n >= 9 ? entries[entries.length - 1] : entries[n - 1]
  if (e) selectEntry(e)
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
