import { defineStore } from 'pinia'
import { reactive } from 'vue'

/**
 * A project pinned as a tab in the workspace. Tabs exist purely to switch
 * quickly between PROJECTS — switching between runs inside a project is still
 * done from RunView's History list, not tabs. Each tab remembers the last run
 * the user was viewing in that project so re-selecting the tab returns there.
 */
export interface WorkspaceTab {
  projectId: string
  lastRunId: string | null
}

const STORAGE_KEY = 'devdy.workspaceTabs'

function load(): WorkspaceTab[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    const arr = raw ? JSON.parse(raw) : []
    return Array.isArray(arr)
      ? arr
          .filter((t): t is { projectId: string; lastRunId?: unknown } => !!t && typeof t.projectId === 'string')
          .map((t) => ({ projectId: t.projectId, lastRunId: typeof t.lastRunId === 'string' ? t.lastRunId : null }))
      : []
  } catch {
    return []
  }
}

export const useWorkspaceTabsStore = defineStore('workspaceTabs', () => {
  const tabs = reactive<WorkspaceTab[]>(load())

  function persist() {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify([...tabs]))
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
    let tab = tabs.find((t) => t.projectId === projectId)
    if (!tab) {
      tab = { projectId, lastRunId: runId ?? null }
      tabs.push(tab)
      persist()
      return
    }
    if (runId && tab.lastRunId !== runId) {
      tab.lastRunId = runId
      persist()
    }
  }

  /**
   * Close a project tab. Returns the neighbouring tab to navigate to (the one
   * that slides into its place, else the previous one), or null if none remain.
   */
  function close(projectId: string): WorkspaceTab | null {
    const idx = tabs.findIndex((t) => t.projectId === projectId)
    if (idx === -1) return null
    tabs.splice(idx, 1)
    persist()
    return tabs[idx] ?? tabs[idx - 1] ?? null
  }

  function has(projectId: string): boolean {
    return tabs.some((t) => t.projectId === projectId)
  }

  /** Forget a deleted run so a tab never resumes onto a run that's gone. */
  function forgetRun(runId: string) {
    let changed = false
    for (const t of tabs) {
      if (t.lastRunId === runId) {
        t.lastRunId = null
        changed = true
      }
    }
    if (changed) persist()
  }

  return { tabs, open, close, has, forgetRun }
})
