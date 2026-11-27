import { defineStore } from 'pinia'
import { computed, reactive, ref } from 'vue'
import { cleanParams, surfaceTabId, type SurfaceKind } from '@/lib/surfaces'
import {
  clampRatio,
  pageTabId,
  parseWorkspaceState,
  pickSession,
  serializeWorkspaceState,
  splitSide,
  tabEntryKey,
  type PageTab,
  type PaneSide,
  type SurfaceTab,
  type TabEntry,
  type WorkspaceTab,
} from '@/lib/workspaceState'

// The entry types live in lib/workspaceState (pure, unit-tested); re-exported
// here for the many callers that import them from the store.
export { pageTabId, tabEntryKey }
export type { PageTab, PaneSide, SurfaceTab, TabEntry, WorkspaceTab }

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
   * Split view: one screen, two modes. Off (full), everything shows full
   * width. On, the left half always shows a session and the right half any
   * other screen (lib/tabActions routes each navigation to its half). Persisted:
   * a restart shows what the user had.
   */
  const splitMode = ref(initial.split)
  /** The session (project tab key) for the left half: the last one viewed. */
  const leftKey = ref<string | null>(initial.left)
  /** The tab (never a session) in the right half; null = an empty half. */
  const rightKey = ref<string | null>(initial.right)
  /** The left pane's share of the main area while split (persisted). */
  const ratio = ref(initial.ratio)

  /** Whether the main area is split right now (the right half may be empty). */
  const isSplit = computed(() => splitMode.value)
  /** Key (`tabEntryKey`) of the tab in the right half while split, else null. */
  const splitId = computed(() => (splitMode.value ? rightKey.value : null))
  /** Which half of a split last took a click — keyboard shortcuts act there. */
  const focusedPane = ref<PaneSide>('left')

  function persist() {
    try {
      localStorage.setItem(
        STORAGE_KEY,
        serializeWorkspaceState({
          entries: [...entries],
          split: splitMode.value,
          left: leftKey.value,
          right: rightKey.value,
          ratio: ratio.value,
        }),
      )
    } catch {
      /* storage unavailable — in-memory tabs still work this session */
    }
  }

  function entry(key: string | null | undefined): TabEntry | null {
    if (!key) return null
    return entries.find((e) => tabEntryKey(e) === key) ?? null
  }

  /** A tab left the strip: the half showing it empties (the caller moves the view on). */
  function forgetKey(key: string) {
    if (rightKey.value === key) {
      rightKey.value = null
      focusedPane.value = 'left'
    }
    if (leftKey.value === key) leftKey.value = null
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
    if (rightKey.value === id) rightKey.value = nextId
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

  /** The session the left half would show: the last one viewed, else any open one. */
  function sessionForLeft(): string | null {
    return pickSession(entries, leftKey.value)
  }

  /** Remember the session on screen (the left half while split). */
  function setLeft(key: string) {
    const e = entry(key)
    if (!e || splitSide(e) !== 'left' || leftKey.value === key) return
    leftKey.value = key
    persist()
  }

  /** Put a tab (never a session) in the right half, or empty it (null). */
  function setRight(key: string | null) {
    const e = entry(key)
    const next = e && splitSide(e) === 'right' ? key : null
    if (rightKey.value === next) return
    rightKey.value = next
    persist()
  }

  /** Turn split view on / off (lib/tabActions moves the router along). */
  function setSplitMode(on: boolean) {
    if (!on) focusedPane.value = 'left'
    if (splitMode.value === on) return
    splitMode.value = on
    persist()
  }

  /** Remember the divider position (`total`: the main area's width, px). */
  function setRatio(next: number, total?: number) {
    const r = clampRatio(next, total ?? 0)
    if (r === ratio.value) return
    ratio.value = r
    persist()
  }

  /** Move the entry at `from` to `to` (drag-and-drop reorder). */
  function move(from: number, to: number) {
    if (from === to) return
    if (from < 0 || from >= entries.length) return
    if (to < 0 || to >= entries.length) return
    const [moved] = entries.splice(from, 1)
    entries.splice(to, 0, moved)
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
    splitMode,
    leftKey,
    rightKey,
    ratio,
    splitId,
    isSplit,
    focusedPane,
    entry,
    open,
    close,
    has,
    move,
    forgetRun,
    surfaceTab,
    openSurface,
    closeSurface,
    retargetSurface,
    pageTab,
    openPage,
    closePage,
    sessionForLeft,
    setLeft,
    setRight,
    setSplitMode,
    setRatio,
  }
})
