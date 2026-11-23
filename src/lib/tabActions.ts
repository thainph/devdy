// Actions on the main window's tab strip (stores/workspaceTabs), shared by the
// strip itself (components/WorkspaceTabs.vue), the command palette and the
// tab shortcuts of the native menu (⌘1…⌘9, close tab, split). Main window only:
// they drive its router.
//
// Split view: any two tabs can show side by side (lib/workspaceState `canPair`).
// The router shows the pair's left half; the right half renders beside it
// (App.vue). Clicking a tab outside the pair shows it full width and leaves the
// pair waiting on the strip; clicking a half brings the split back.
import { nextTick } from 'vue'
import type { RouteLocationNormalizedLoaded } from 'vue-router'
import router from '@/router'
import { tabEntryKey, useWorkspaceTabsStore, type TabEntry } from '@/stores/workspaceTabs'
import { useRunsStore } from '@/stores/runs'
import { surfaceForPath } from '@/lib/surfaces'
import { pageRouteFor } from '@/lib/navigation'
import { entryTargetFor, entryTargetKey, withOwnRun, type EntryTarget } from '@/lib/workspaceState'
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
export function sanitized(e: TabEntry): TabEntry {
  const runs = useRunsStore()
  return withOwnRun(e, (runId) => runs.runMeta.get(runId)?.project_id)
}

/**
 * Startup: the router's first navigation (`/` → `/dashboard`) is not the user
 * choosing a tab, so it must not change the split restored from the last
 * session (stores/workspaceTabs `pairShown`). Until it has landed, the pair
 * neither follows the route nor diverts it; then a restored split routes to
 * its left half.
 */
let startupSettled = false

/** Main window, once: lift the startup gate (see above). */
export async function settleStartupNavigation() {
  await router.isReady()
  // Let the route watcher handle the first navigation while still gated.
  await nextTick()
  startupSettled = true
  if (useWorkspaceTabsStore().isSplit) routeToLeft()
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
 * Close every tab but `e`, which is left on screen full width (any split ends:
 * its other half is closed too). The others close straight in the store, so the
 * router moves once, to `e`, instead of hopping through each neighbour.
 */
export async function closeOtherEntries(e: TabEntry) {
  const tabs = useWorkspaceTabsStore()
  const keep = tabEntryKey(e)
  tabs.unsplit()
  if (keep !== routedKey()) await goToEntry(sanitized(e))
  for (const other of [...tabs.entries]) {
    if (tabEntryKey(other) === keep) continue
    if (other.type === 'project') tabs.close(other.projectId)
    else if (other.type === 'page') tabs.closePage(other.id)
    else tabs.closeSurface(other.id)
  }
}

/** The tab on the left half: the pair's left when split, else what's on screen. */
function leftKey(): string | null {
  const tabs = useWorkspaceTabsStore()
  return tabs.isSplit ? (tabs.pair?.left ?? null) : routedTargetKey()
}

/**
 * Split view has one rule: the left half is the screen you're on, the right
 * half is the tab you open on the right. So "Open on the right" works on any
 * tab but the one on the left (or already on the right) that may sit beside it.
 */
export function canOpenOnRight(e: TabEntry): boolean {
  const tabs = useWorkspaceTabsStore()
  const key = tabEntryKey(e)
  const left = leftKey()
  if (!left || key === left) return false
  if (tabs.isSplit && tabs.pair?.right === key) return false
  return tabs.canSplit(left, key)
}

/** Show `e` in the right half: opens the split, or replaces the right half. */
export function openOnRight(e: TabEntry) {
  if (!canOpenOnRight(e)) return
  const tabs = useWorkspaceTabsStore()
  const key = tabEntryKey(e)
  if (tabs.isSplit) {
    if (tabs.replaceSide('right', key)) tabs.focusedPane = 'right'
    return
  }
  const left = leftKey()
  if (left) tabs.setPair(left, key, 'right')
}

/** Swap the two halves; the focused tab keeps the focus on its new side. */
export function swapSplit() {
  const tabs = useWorkspaceTabsStore()
  const p = tabs.pair
  if (!p || !tabs.isSplit) return
  if (tabs.setPair(p.right, p.left, tabs.focusedPane === 'left' ? 'right' : 'left')) routeToLeft()
}

/** Close the split, keeping the focused half on screen (full width). */
export function unsplitFocused() {
  const tabs = useWorkspaceTabsStore()
  const right = tabs.focusedPane === 'right' ? tabs.entry(tabs.splitId) : null
  tabs.unsplit()
  if (right) void goToEntry(sanitized(right))
}

/** The last pair, waiting on the strip while another tab is shown, if any. */
export function idlePair(): { left: TabEntry; right: TabEntry } | null {
  const tabs = useWorkspaceTabsStore()
  if (tabs.isSplit || !tabs.pair) return null
  const left = tabs.entry(tabs.pair.left)
  const right = tabs.entry(tabs.pair.right)
  return left && right ? { left, right } : null
}

/** ⌘\ — close the split, or bring the last one back. Never guesses a tab. */
export function toggleSplitCurrent() {
  const tabs = useWorkspaceTabsStore()
  if (tabs.isSplit) {
    unsplitFocused()
    return
  }
  const pair = idlePair()
  if (pair) selectEntry(pair.left)
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
  if (!startupSettled) return true
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
  if (!startupSettled) return
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
