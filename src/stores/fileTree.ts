import { defineStore } from 'pinia'
import { reactive } from 'vue'
import { useRunsStore, type DirEntry } from '@/stores/runs'

// Per-project state for the VSCode-style file-tree panel. Directories are loaded
// lazily (one level at a time) when the user expands a node; loaded children are
// cached so re-expanding never re-hits the backend until an explicit refresh.
interface ProjectTree {
  /** Relative dir paths (POSIX) that are currently expanded. `""` = root. */
  expanded: Set<string>
  /** Loaded direct children per dir path (cache). */
  children: Record<string, DirEntry[]>
  /** Dir paths whose children are currently being fetched. */
  loading: Set<string>
  /** Load error message per dir path, if any. */
  error: Record<string, string>
}

export const useFileTreeStore = defineStore('fileTree', () => {
  const runs = useRunsStore()

  // Keyed by project.path so each open project keeps its own expanded state.
  const trees = reactive<Record<string, ProjectTree>>({})

  function treeFor(projectPath: string): ProjectTree {
    if (!trees[projectPath]) {
      trees[projectPath] = { expanded: new Set(), children: {}, loading: new Set(), error: {} }
    }
    // Return the reactive proxy (not the raw literal) so later Set/object
    // mutations trigger re-renders — otherwise the panel spins forever.
    return trees[projectPath]
  }

  // Load one directory level. Uses the cache unless `force` is set.
  async function loadDir(projectPath: string, relDir: string, force = false): Promise<void> {
    const t = treeFor(projectPath)
    if (!force && t.children[relDir]) return
    if (t.loading.has(relDir)) return
    t.loading.add(relDir)
    delete t.error[relDir]
    try {
      t.children[relDir] = await runs.listDir(projectPath, relDir)
    } catch (e) {
      t.error[relDir] = String(e)
    } finally {
      t.loading.delete(relDir)
    }
  }

  // Expand/collapse a directory, lazy-loading its children on first expand.
  async function toggle(projectPath: string, relDir: string): Promise<void> {
    const t = treeFor(projectPath)
    if (t.expanded.has(relDir)) {
      t.expanded.delete(relDir)
    } else {
      t.expanded.add(relDir)
      await loadDir(projectPath, relDir)
    }
  }

  // Drop cached children and reload. Without `relDir`, resets the whole tree to
  // just the (reloaded) root; with one, reloads that subtree in place.
  async function refresh(projectPath: string, relDir = ''): Promise<void> {
    const t = treeFor(projectPath)
    if (relDir === '') {
      t.children = {}
      t.error = {}
      // Keep only still-relevant expanded paths (root always).
      t.expanded = new Set([''])
    } else {
      delete t.children[relDir]
    }
    await loadDir(projectPath, relDir, true)
  }

  function collapseAll(projectPath: string): void {
    const t = treeFor(projectPath)
    t.expanded = new Set([''])
  }

  return { trees, treeFor, loadDir, toggle, refresh, collapseAll }
})
