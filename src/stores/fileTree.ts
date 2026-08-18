import { defineStore } from 'pinia'
import { reactive } from 'vue'
import { listen } from '@tauri-apps/api/event'
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

/** Parent directory (relative POSIX) of a relative path; `''` for a top-level entry. */
function parentOf(relPath: string): string {
  const i = relPath.lastIndexOf('/')
  return i === -1 ? '' : relPath.slice(0, i)
}
/** Basename of a relative path. */
function baseOf(relPath: string): string {
  const i = relPath.lastIndexOf('/')
  return i === -1 ? relPath : relPath.slice(i + 1)
}

export const useFileTreeStore = defineStore('fileTree', () => {
  const runs = useRunsStore()

  // Keyed by project.path so each open project keeps its own expanded state.
  const trees = reactive<Record<string, ProjectTree>>({})

  // Path of the entry currently being renamed inline, per project (null = none).
  const renaming = reactive<Record<string, string | null>>({})

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

  // Reload just one directory's direct children in place, preserving expansion
  // state and every other cached subtree (unlike `refresh('')`, which resets the
  // whole tree). Used after a mutation so the affected folder updates smoothly.
  async function reloadDir(projectPath: string, relDir: string): Promise<void> {
    const t = treeFor(projectPath)
    delete t.error[relDir]
    await loadDir(projectPath, relDir, true)
  }

  // ── Live FS watch (backend `set/stop_file_tree_watch`) ─────────────────────
  // The backend watches only the currently expanded+loaded dirs of an open tree
  // and emits `file_tree:changed` on external changes. Registered once for the
  // whole app: reload the touched dir only if we already have it cached (i.e. it
  // is visible), so a change to a collapsed/unknown folder is cheaply ignored.
  listen<{ project_path: string; dir: string }>('file_tree:changed', (e) => {
    const { project_path, dir } = e.payload
    const t = trees[project_path]
    if (t && dir in t.children) void reloadDir(project_path, dir)
  })

  // ── Mutations (context-menu / toolbar / drag-and-drop) ─────────────────────
  // Each calls the backend, then reloads (and expands) the affected folder so the
  // change appears without a full-tree refresh. Errors propagate to the caller.
  async function createDir(projectPath: string, relDir: string, name: string): Promise<string> {
    const p = await runs.createDir(projectPath, relDir, name)
    treeFor(projectPath).expanded.add(relDir)
    await reloadDir(projectPath, relDir)
    return p
  }
  async function createFile(projectPath: string, relDir: string, name: string): Promise<string> {
    const p = await runs.createFile(projectPath, relDir, name)
    treeFor(projectPath).expanded.add(relDir)
    await reloadDir(projectPath, relDir)
    return p
  }
  async function rename(projectPath: string, relPath: string, newName: string): Promise<string> {
    const p = await runs.renameEntry(projectPath, relPath, newName)
    await reloadDir(projectPath, parentOf(relPath))
    return p
  }
  async function remove(projectPath: string, relPath: string): Promise<void> {
    await runs.deleteEntry(projectPath, relPath)
    await reloadDir(projectPath, parentOf(relPath))
  }
  async function copyInto(projectPath: string, srcRel: string, destDir: string): Promise<string> {
    const p = await runs.copyEntry(projectPath, srcRel, destDir)
    treeFor(projectPath).expanded.add(destDir)
    await reloadDir(projectPath, destDir)
    return p
  }
  async function moveInto(projectPath: string, srcRel: string, destDir: string): Promise<string> {
    const srcParent = parentOf(srcRel)
    if (destDir === srcParent) return srcRel // no-op
    const p = await runs.moveEntry(projectPath, srcRel, destDir)
    treeFor(projectPath).expanded.add(destDir)
    await reloadDir(projectPath, srcParent)
    await reloadDir(projectPath, destDir)
    return p
  }
  async function duplicate(projectPath: string, relPath: string): Promise<string> {
    return copyInto(projectPath, relPath, parentOf(relPath))
  }

  // ── Inline rename state ────────────────────────────────────────────────────
  function beginRename(projectPath: string, relPath: string): void {
    renaming[projectPath] = relPath
  }
  function cancelRename(projectPath: string): void {
    renaming[projectPath] = null
  }
  async function commitRename(projectPath: string, relPath: string, newName: string): Promise<void> {
    renaming[projectPath] = null
    const trimmed = newName.trim()
    if (!trimmed || trimmed === baseOf(relPath)) return
    await rename(projectPath, relPath, trimmed)
  }

  return {
    trees, renaming, treeFor, loadDir, toggle, refresh, reloadDir, collapseAll,
    createDir, createFile, rename, remove, copyInto, moveInto, duplicate,
    beginRename, cancelRename, commitRename,
  }
})
