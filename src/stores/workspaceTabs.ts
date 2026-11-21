import { defineStore } from 'pinia'
import { computed, reactive, ref } from 'vue'
import {
  cleanParams,
  isSurfaceKind,
  surfaceTabId,
  type SurfaceKind,
  type SurfaceParams,
} from '@/lib/surfaces'

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

/** One entry of the shared tab strip, in display order. */
export type TabEntry =
  | ({ type: 'project' } & WorkspaceTab)
  | ({ type: 'surface' } & SurfaceTab)

const STORAGE_KEY = 'devdy.workspaceTabs'

export function tabEntryKey(e: TabEntry): string {
  return e.type === 'project' ? `project:${e.projectId}` : e.id
}

function load(): TabEntry[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    const arr = raw ? JSON.parse(raw) : []
    if (!Array.isArray(arr)) return []
    const out: TabEntry[] = []
    for (const t of arr) {
      if (!t || typeof t !== 'object') continue
      // Entries saved before surface tabs existed have no `type`: projects.
      if ((t.type === 'project' || t.type === undefined) && typeof t.projectId === 'string') {
        out.push({
          type: 'project',
          projectId: t.projectId,
          lastRunId: typeof t.lastRunId === 'string' ? t.lastRunId : null,
        })
      } else if (t.type === 'surface' && isSurfaceKind(t.surface)) {
        const params = cleanParams(t.surface, t.params)
        out.push({ type: 'surface', id: surfaceTabId(t.surface, params), surface: t.surface, params })
      }
    }
    return out
  } catch {
    return []
  }
}

export const useWorkspaceTabsStore = defineStore('workspaceTabs', () => {
  const entries = reactive<TabEntry[]>(load())

  /** Project tabs only, in strip order (what most callers care about). */
  const tabs = computed(() =>
    entries.filter((e): e is { type: 'project' } & WorkspaceTab => e.type === 'project'),
  )

  /**
   * The surface tab shown in the right half of a split view, or null when the
   * main area isn't split. Only surfaces split: a session needs the router (and
   * its single conductor panel), so it always stays on the left.
   */
  const splitId = ref<string | null>(null)
  /** Which half of a split last took a click — tab clicks land there. */
  const focusedPane = ref<'left' | 'right'>('left')

  function persist() {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify([...entries]))
    } catch {
      /* storage unavailable — in-memory tabs still work this session */
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

  /**
   * Close a project tab. Returns the neighbouring entry to navigate to (the one
   * that slides into its place, else the previous one), or null if none remain.
   */
  function close(projectId: string): TabEntry | null {
    const idx = entries.findIndex((t) => t.type === 'project' && t.projectId === projectId)
    if (idx === -1) return null
    entries.splice(idx, 1)
    persist()
    return neighbour(idx)
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
      if (splitId.value === id) splitId.value = nextId
      persist()
      return surfaceTab(nextId)
    }
    entries.splice(idx, 1, { type: 'surface', id: nextId, surface: old.surface, params })
    if (splitId.value === id) splitId.value = nextId
    persist()
    return surfaceTab(nextId)
  }

  /** Close a surface tab; returns its neighbour like `close`. */
  function closeSurface(id: string): TabEntry | null {
    const idx = entries.findIndex((t) => t.type === 'surface' && t.id === id)
    if (idx === -1) return null
    entries.splice(idx, 1)
    if (splitId.value === id) unsplit()
    persist()
    return neighbour(idx)
  }

  /** Show a surface tab in the right half (opening the split if needed). */
  function split(id: string) {
    if (!surfaceTab(id)) return
    splitId.value = id
    focusedPane.value = 'right'
  }

  function unsplit() {
    splitId.value = null
    focusedPane.value = 'left'
  }

  /**
   * Move the entry at `from` to `to` (drag-and-drop reorder). Called repeatedly
   * while a drag is in flight, so the tab slides under the cursor live.
   */
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
    splitId,
    focusedPane,
    open,
    close,
    has,
    move,
    forgetRun,
    surfaceTab,
    openSurface,
    closeSurface,
    retargetSurface,
    split,
    unsplit,
  }
})
