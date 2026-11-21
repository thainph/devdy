// Actions on the main window's tab strip (stores/workspaceTabs), shared by the
// strip itself (components/WorkspaceTabs.vue), the command palette and the
// tab shortcuts of the native menu (⌘1…⌘9, close tab, split). Main window only:
// they drive its router.
//
// Split view: any two tabs can show side by side (lib/workspaceState `canPair`).
// The router shows the pair's left half; the right half renders beside it
// (App.vue). Clicking a tab outside the pair shows it full width and leaves the
// pair waiting on the strip; clicking a half brings the split back.
import type { RouteLocationNormalizedLoaded } from 'vue-router'
import router from '@/router'
import { tabEntryKey, useWorkspaceTabsStore, type TabEntry } from '@/stores/workspaceTabs'
import { useRunsStore } from '@/stores/runs'
import { surfaceForPath } from '@/lib/surfaces'
import { pageRouteFor } from '@/lib/navigation'
import { entryTargetFor, entryTargetKey, type EntryTarget, type PaneSide } from '@/lib/workspaceState'
import {
  goToEntry,
  openSurface,
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
  if (tabs.focusedPane === 'right' && tabs.splitId) return tabs.entry(tabs.splitId)
  return tabs.entry(routedKey())
}

/** A project tab remembering a run of another project resumes on the project instead. */
function sanitized(e: TabEntry): TabEntry {
  if (e.type !== 'project' || !e.lastRunId) return e
  // A remembered run from another project (pinned before cross-project workers
  // were filed correctly) would open that project's session under this tab.
  const owner = useRunsStore().runMeta.get(e.lastRunId)?.project_id
  return owner && owner !== e.projectId ? { ...e, lastRunId: null } : e
}

/** Make the router show the pair's left half (it is what the left pane shows). */
function routeToLeft() {
  const tabs = useWorkspaceTabsStore()
  const left = tabs.entry(tabs.pair?.left)
  if (left && routedTargetKey() !== tabs.pair?.left) void goToEntry(sanitized(left))
}

/** Go to a tab, as a click on it in the strip does. */
export function selectEntry(e: TabEntry) {
  const tabs = useWorkspaceTabsStore()
  const key = tabEntryKey(e)
  const pair = tabs.pair
  if (pair && (key === pair.left || key === pair.right)) {
    // A half of the pair: the split comes back with that half focused.
    tabs.showPair(key === pair.left ? 'left' : 'right')
    routeToLeft()
    return
  }
  // Any other tab shows full width; the pair waits on the strip.
  tabs.hidePair()
  if (key === routedKey()) return
  void goToEntry(sanitized(e))
}

/** Close a tab; when it was on screen, move to its neighbour (or the other half). */
export function closeEntry(e: TabEntry) {
  void removeTab(e)
}

/**
 * The tab to show beside `key` when splitting it with nothing chosen: the
 * right-most surface (a session beside its PR Inbox / Todos / file), else the
 * nearest neighbour on the strip that may pair with it.
 */
export function splitCandidateFor(key: string | null): TabEntry | null {
  const tabs = useWorkspaceTabsStore()
  if (!key) return null
  const ok = tabs.entries.filter((x) => tabs.canSplit(key, tabEntryKey(x)))
  const surfaces = ok.filter((x) => x.type === 'surface')
  if (surfaces.length) return surfaces[surfaces.length - 1]
  const at = tabs.entries.findIndex((x) => tabEntryKey(x) === key)
  let best: TabEntry | null = null
  let dist = Infinity
  for (const x of ok) {
    const d = Math.abs(tabs.entries.indexOf(x) - at)
    if (d < dist) {
      best = x
      dist = d
    }
  }
  return best
}

/** What ⌘\ would split the current tab with (null when it can't, or when already split). */
export function splitCandidate(): TabEntry | null {
  const tabs = useWorkspaceTabsStore()
  if (tabs.isSplit) return null
  return splitCandidateFor(routedTargetKey())
}

/** Put `key` in one half of the shown split, routing the left half there. */
function placeInSide(side: PaneSide, key: string): boolean {
  const tabs = useWorkspaceTabsStore()
  if (!tabs.replaceSide(side, key)) return false
  tabs.focusedPane = side
  if (side === 'left') routeToLeft()
  return true
}

/**
 * Whether the tab's split button does something: on a half of the pair it
 * closes the split; on any other tab it opens it beside what's on screen (in
 * the half that isn't focused, when already split).
 */
export function canSplitEntry(e: TabEntry): boolean {
  const tabs = useWorkspaceTabsStore()
  const key = tabEntryKey(e)
  const pair = tabs.pair
  if (pair && tabs.pairShown && (key === pair.left || key === pair.right)) return true
  if (tabs.isSplit && pair) return tabs.canSplit(pair[tabs.focusedPane], key)
  const routed = routedTargetKey()
  if (!routed) return false
  return routed === key ? !!splitCandidateFor(key) : tabs.canSplit(routed, key)
}

/** The split button of a tab (see `canSplitEntry`). */
export function toggleSplitEntry(e: TabEntry) {
  const tabs = useWorkspaceTabsStore()
  const key = tabEntryKey(e)
  const pair = tabs.pair
  if (pair && tabs.pairShown && (key === pair.left || key === pair.right)) {
    unsplitFocused()
    return
  }
  if (tabs.isSplit && pair) {
    placeInSide(tabs.focusedPane === 'right' ? 'left' : 'right', key)
    return
  }
  const routed = routedTargetKey()
  if (!routed) return
  if (routed !== key) {
    tabs.setPair(routed, key, 'right')
    return
  }
  const other = splitCandidateFor(key)
  if (other) tabs.setPair(key, tabEntryKey(other), 'right')
}

/** Close the split, keeping the focused half on screen (full width). */
export function unsplitFocused() {
  const tabs = useWorkspaceTabsStore()
  const right = tabs.focusedPane === 'right' ? tabs.entry(tabs.splitId) : null
  tabs.unsplit()
  if (right) void goToEntry(sanitized(right))
}

/** ⌘\ — close the split; bring a waiting pair back; or split the current tab. */
export function toggleSplitCurrent() {
  const tabs = useWorkspaceTabsStore()
  if (tabs.isSplit) {
    unsplitFocused()
    return
  }
  const pair = tabs.pair
  const routed = routedTargetKey()
  if (pair && routed && (routed === pair.left || routed === pair.right)) {
    selectEntry(tabs.entry(routed)!)
    return
  }
  const target = splitCandidate()
  if (routed && target) tabs.setPair(routed, tabEntryKey(target), 'right')
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

// --- Navigation into the right half -------------------------------------------

function targetOf(to: RouteLocationNormalizedLoaded): EntryTarget | null {
  return entryTargetFor(to, surfaceForPath, (p) => !!pageRouteFor(p))
}

/** Open / update the tab of a route target; returns its key. A run filed under another project moves there. */
function openTarget(t: EntryTarget): string {
  const tabs = useWorkspaceTabsStore()
  if (t.type === 'project') {
    const owner = t.runId ? useRunsStore().runMeta.get(t.runId)?.project_id : undefined
    const projectId = owner && owner !== t.projectId ? owner : t.projectId
    tabs.open(projectId, t.runId)
    return `project:${projectId}`
  }
  if (t.type === 'page') return tabs.openPage(t.path, t.fullPath).id
  return tabs.openSurface(t.surface, t.params).id
}

/**
 * A screen in the right half navigated (PaneRouterView's push / replace). Stays
 * in the right half when it can: the same tab moves along, another tab takes
 * the half's place. Screens that aren't tabs (a project's settings), or that
 * can't sit beside the left half, show in the main pane instead.
 */
export function navigateRightPane(to: RouteLocationNormalizedLoaded) {
  const tabs = useWorkspaceTabsStore()
  const target = targetOf(to)
  if (!target) {
    tabs.focusedPane = 'left'
    void router.push(to.fullPath).catch(() => {})
    return
  }
  if (target.type === 'surface') {
    // Surfaces may open as their own window (Settings → Windows & tabs).
    void openSurface(target.surface, target.params, tabs.isSplit ? { pane: 'right' } : {})
    return
  }
  const key = openTarget(target)
  if (key === tabs.pair?.left || !tabs.isSplit || !tabs.replaceSide('right', key)) {
    if (key !== tabs.pair?.left) tabs.hidePair()
    else tabs.focusedPane = 'left'
    void router.push(to.fullPath).catch(() => {})
    return
  }
  rightPaneLocation.value = { key, fullPath: to.fullPath }
  tabs.focusedPane = 'right'
}

/**
 * Global navigation guard (main window): a navigation to the tab in the
 * right half of the pair shows it THERE — never on both sides — and brings the
 * split on screen. Returns false to cancel the router's navigation.
 */
export function interceptRightHalf(to: RouteLocationNormalizedLoaded): boolean {
  const tabs = useWorkspaceTabsStore()
  const pair = tabs.pair
  if (!pair) return true
  const target = targetOf(to)
  if (!target || entryTargetKey(target) !== pair.right) return true
  const key = openTarget(target)
  if (key !== pair.right) return true
  if (target.type !== 'surface') rightPaneLocation.value = { key, fullPath: to.fullPath }
  tabs.showPair('right')
  // After this navigation is cancelled, not from inside its guard.
  setTimeout(routeToLeft, 0)
  return false
}

/**
 * The router landed somewhere (App.vue's route watcher). On the pair's left
 * half, the pair comes back on screen; on another tab while split, that tab
 * takes the left half (a link followed inside it) — or, when it can't sit
 * beside the right half, shows full width.
 */
export function syncPairWithRoute() {
  const tabs = useWorkspaceTabsStore()
  const pair = tabs.pair
  if (!pair) return
  const key = routedTargetKey()
  if (!key) return
  if (key === pair.left) {
    if (!tabs.pairShown) tabs.showPair('left')
    return
  }
  if (tabs.isSplit && !tabs.replaceSide('left', key)) tabs.hidePair()
}

/**
 * The sidebar's links: in the focused half of a split (so the right half can
 * be steered from there too), else the main pane.
 */
export function navigateFocused(path: string) {
  const tabs = useWorkspaceTabsStore()
  if (tabs.isSplit && tabs.focusedPane === 'right') {
    navigateRightPane(router.resolve(path) as unknown as RouteLocationNormalizedLoaded)
    return
  }
  void router.push(path).catch(() => {})
}
