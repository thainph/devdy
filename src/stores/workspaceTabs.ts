import { defineStore } from 'pinia'
import { computed, reactive, ref } from 'vue'
import { cleanParams, surfaceTabId, type SurfaceKind } from '@/lib/surfaces'
import {
  canPair,
  clampRatio,
  moveUnit as moveUnitOrder,
  pageTabId,
  parseWorkspaceState,
  placePair,
  replacePairSide,
  sanitizeRatio,
  serializeWorkspaceState,
  tabEntryKey,
  type PageTab,
  type PaneSide,
  type SplitPair,
  type SurfaceTab,
  type TabEntry,
  type WorkspaceTab,
} from '@/lib/workspaceState'

// The entry types live in lib/workspaceState (pure, unit-tested); re-exported
// here for the many callers that import them from the store.
export { pageTabId, tabEntryKey }
export type { PageTab, PaneSide, SplitPair, SurfaceTab, TabEntry, WorkspaceTab }

const STORAGE_KEY = 'devdy.workspaceTabs'

function load() {
  try {
    return parseWorkspaceState(localStorage.getItem(STORAGE_KEY))
  } catch {
    return parseWorkspaceState(null)
  }
}

export const useWorkspaceTabsStore = defineStore('workspaceTabs', () => {
  const initial = load()
  const entries = reactive<TabEntry[]>(initial.entries)

  /** Project tabs only, in strip order (what most callers care about). */
  const tabs = computed(() =>
    entries.filter((e): e is { type: 'project' } & WorkspaceTab => e.type === 'project'),
  )

  /**
   * The two tabs of the split view (Chrome-style: adjacent on the strip, drawn
   * as one tab), or null. Any two tabs pair (lib/workspaceState `canPair`):
   * the left half is what the router shows, the right half is rendered beside
   * it (App.vue — a surface host or a pane-scoped router view).
   *
   * The pair outlives being on screen: clicking a tab outside it shows that tab
   * full width while the pair waits on the strip (`pairShown` false) until one
   * of its halves is clicked again.
   */
  const pair = ref<SplitPair | null>(initial.pair)
  /** Whether the pair is on screen. Not persisted: a restart shows one tab. */
  const pairShown = ref(false)
  /** The left pane's share of the main area while split (persisted). */
  const ratio = ref(initial.ratio)

  /**
   * Key (`tabEntryKey`) of the tab in the right half, or null when the main
   * area isn't split. Non-null exactly while the split is on screen.
   */
  const splitId = computed(() => (pair.value && pairShown.value ? pair.value.right : null))
  /** Whether the main area is split right now. */
  const isSplit = computed(() => splitId.value != null)
  /** Which half of a split last took a click — tab clicks land there. */
  const focusedPane = ref<PaneSide>('left')

  function persist() {
    try {
      localStorage.setItem(
        STORAGE_KEY,
        serializeWorkspaceState({ entries: [...entries], pair: pair.value, ratio: ratio.value }),
      )
    } catch {
      /* storage unavailable — in-memory tabs still work this session */
    }
  }

  function entry(key: string | null | undefined): TabEntry | null {
    if (!key) return null
    return entries.find((e) => tabEntryKey(e) === key) ?? null
  }

  /** Reorder the strip to `keys` (a permutation of the current keys). */
  function applyOrder(keys: string[]) {
    const byKey = new Map(entries.map((e) => [tabEntryKey(e), e]))
    const next = keys.map((k) => byKey.get(k)).filter((e): e is TabEntry => !!e)
    if (next.length !== entries.length) return
    if (next.every((e, i) => e === entries[i])) return
    entries.splice(0, entries.length, ...next)
  }

  /** A tab left the strip: a pair it was half of dissolves. */
  function forgetKey(key: string) {
    if (pair.value && (pair.value.left === key || pair.value.right === key)) {
      pair.value = null
      pairShown.value = false
      focusedPane.value = 'left'
    }
  }

  /**
   * Pin a project as a tab (idempotent) and, when a run is given, record it as
   * the project's last-viewed run so the tab resumes there next time.
   */
  function open(projectId: string, runId?: string | null) {
    if (!projectId) return
    const tab = tabs.value.find((t) => t.projectId === projectId)
    if (!tab) {
      entries.push({ type: 'project', projectId, lastRunId: runId ?? null })
      persist()
      return
    }
    if (runId && tab.lastRunId !== runId) {
      tab.lastRunId = runId
      persist()
    }
  }

  /** Index of the entry that slid into a closed one's place, else the one before. */
  function neighbour(idx: number): TabEntry | null {
    return entries[idx] ?? entries[idx - 1] ?? null
  }

  function removeAt(idx: number): TabEntry | null {
    const [gone] = entries.splice(idx, 1)
    if (gone) forgetKey(tabEntryKey(gone))
    persist()
    return neighbour(idx)
  }

  /**
   * Close a project tab. Returns the neighbouring entry to navigate to (the one
   * that slides into its place, else the previous one), or null if none remain.
   */
  function close(projectId: string): TabEntry | null {
    const idx = entries.findIndex((t) => t.type === 'project' && t.projectId === projectId)
    return idx === -1 ? null : removeAt(idx)
  }

  function has(projectId: string): boolean {
    return tabs.value.some((t) => t.projectId === projectId)
  }

  function surfaceTab(id: string): SurfaceTab | null {
    const e = entries.find((t) => t.type === 'surface' && t.id === id)
    return e && e.type === 'surface' ? e : null
  }

  /**
   * Open (or re-point) the tab of a surface. Surfaces have one tab each (files
   * one per file), so opening Todos on another todo updates the Todos tab.
   * Returns the tab.
   */
  function openSurface(surface: SurfaceKind, rawParams?: Record<string, unknown>): SurfaceTab {
    const params = cleanParams(surface, rawParams)
    const id = surfaceTabId(surface, params)
    const existing = surfaceTab(id)
    if (existing) {
      if (JSON.stringify(existing.params) !== JSON.stringify(params)) {
        existing.params = params
        persist()
      }
      return existing
    }
    entries.push({ type: 'surface', id, surface, params })
    persist()
    return surfaceTab(id)!
  }

  /**
   * Point a surface tab at new params in place (a renamed file): same strip
   * position, same half of the split. Returns the tab (its id may change).
   */
  function retargetSurface(id: string, rawParams: Record<string, unknown>): SurfaceTab | null {
    const idx = entries.findIndex((t) => t.type === 'surface' && t.id === id)
    if (idx === -1) return null
    const old = entries[idx] as { type: 'surface' } & SurfaceTab
    const params = cleanParams(old.surface, rawParams)
    const nextId = surfaceTabId(old.surface, params)
    // Already open under the new identity: just drop the old tab.
    if (nextId !== id && surfaceTab(nextId)) {
      entries.splice(idx, 1)
      forgetKey(id)
      persist()
      return surfaceTab(nextId)
    }
    entries.splice(idx, 1, { type: 'surface', id: nextId, surface: old.surface, params })
    if (pair.value?.left === id) pair.value = { ...pair.value, left: nextId }
    if (pair.value?.right === id) pair.value = { ...pair.value, right: nextId }
    persist()
    return surfaceTab(nextId)
  }

  function pageTab(id: string): PageTab | null {
    const e = entries.find((t) => t.type === 'page' && t.id === id)
    return e && e.type === 'page' ? e : null
  }

  /** Open (or update) the tab of a page, remembering where in it the user is. */
  function openPage(path: string, fullPath: string = path): PageTab {
    const id = pageTabId(path)
    const existing = pageTab(id)
    if (existing) {
      if (existing.fullPath !== fullPath) {
        existing.fullPath = fullPath
        persist()
      }
      return existing
    }
    entries.push({ type: 'page', id, path, fullPath })
    persist()
    return pageTab(id)!
  }

  /** Close a page tab; returns its neighbour like `close`. */
  function closePage(id: string): TabEntry | null {
    const idx = entries.findIndex((t) => t.type === 'page' && t.id === id)
    return idx === -1 ? null : removeAt(idx)
  }

  /** Close a surface tab; returns its neighbour like `close`. */
  function closeSurface(id: string): TabEntry | null {
    const idx = entries.findIndex((t) => t.type === 'surface' && t.id === id)
    return idx === -1 ? null : removeAt(idx)
  }

  /** Whether the tabs `a` and `b` (keys) may be split side by side. */
  function canSplit(a: string | null | undefined, b: string | null | undefined): boolean {
    const x = entry(a)
    const y = entry(b)
    return !!x && !!y && canPair(x, y)
  }

  /**
   * Show `left` and `right` side by side (the router must show `left`; the
   * caller navigates). Replaces any previous pair, moves `right` next to `left`
   * on the strip. Returns false when the two can't pair.
   */
  function setPair(left: string, right: string, focus: PaneSide = 'right'): boolean {
    if (!canSplit(left, right)) return false
    pair.value = { left, right }
    applyOrder(placePair(entries.map(tabEntryKey), pair.value))
    pairShown.value = true
    focusedPane.value = focus
    persist()
    return true
  }

  /**
   * Put `key` in one half of the shown pair (a navigation inside that half
   * landed on another tab). The replaced tab stays open next to the pair.
   * Returns false when it can't pair with the other half.
   */
  function replaceSide(side: PaneSide, key: string): boolean {
    const p = pair.value
    if (!p) return false
    if (p[side] === key) return true
    if (!canSplit(side === 'left' ? p.right : p.left, key)) return false
    const next = replacePairSide(entries.map(tabEntryKey), p, side, key)
    if (!next) return false
    pair.value = next.pair
    applyOrder(next.keys)
    persist()
    return true
  }

  /** Bring the pair back on screen (the caller routes to its left half). */
  function showPair(focus: PaneSide = 'left') {
    if (!pair.value) return
    pairShown.value = true
    focusedPane.value = focus
  }

  /** Show one tab full width; the pair, if any, stays on the strip. */
  function hidePair() {
    pairShown.value = false
    focusedPane.value = 'left'
  }

  /** Dissolve the pair: both halves become ordinary tabs again. */
  function unsplit() {
    if (pair.value) {
      pair.value = null
      persist()
    }
    pairShown.value = false
    focusedPane.value = 'left'
  }

  /** Remember the divider position (`total`: the main area's width, px). */
  function setRatio(next: number, total?: number) {
    const r = total ? clampRatio(next, total) : sanitizeRatio(next)
    if (r === ratio.value) return
    ratio.value = r
    persist()
  }

  /**
   * Move the entry at `from` to `to` (drag-and-drop reorder). A pair keeps its
   * halves together: use `moveUnit` for the strip's drawn units.
   */
  function move(from: number, to: number) {
    if (from === to) return
    if (from < 0 || from >= entries.length) return
    if (to < 0 || to >= entries.length) return
    const [moved] = entries.splice(from, 1)
    entries.splice(to, 0, moved)
    if (pair.value) applyOrder(placePair(entries.map(tabEntryKey), pair.value))
    persist()
  }

  /** Move drawn unit `from` to unit slot `to` (lib/workspaceState `stripUnits`). */
  function moveUnit(from: number, to: number) {
    applyOrder(moveUnitOrder(entries.map(tabEntryKey), pair.value, from, to))
    persist()
  }

  /** Forget a deleted run so a tab never resumes onto a run that's gone. */
  function forgetRun(runId: string) {
    let changed = false
    for (const t of tabs.value) {
      if (t.lastRunId === runId) {
        t.lastRunId = null
        changed = true
      }
    }
    if (changed) persist()
  }

  return {
    entries,
    tabs,
    pair,
    pairShown,
    ratio,
    splitId,
    isSplit,
    focusedPane,
    entry,
    open,
    close,
    has,
    move,
    moveUnit,
    forgetRun,
    surfaceTab,
    openSurface,
    closeSurface,
    retargetSurface,
    pageTab,
    openPage,
    closePage,
    canSplit,
    setPair,
    replaceSide,
    showPair,
    hidePair,
    unsplit,
    setRatio,
  }
})
