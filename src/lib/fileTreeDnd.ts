// Drag-and-drop for the file explorer.
//
// HTML5 DnD can't be used: Tauri's native drag-drop handler (dragDropEnabled,
// on by default so the composer can receive OS file drops) claims EVERY drag
// over the webview on macOS — including in-page ones — so `dragover`/`drop`
// never reach the DOM. Instead:
//   • moving entries inside the tree is a pointer-driven drag (startTreeDrag);
//   • files dropped from Finder arrive via the native onDragDropEvent, and the
//     tree hit-tests the reported position (treeDropTargetAt);
//   • an in-tree drag that leaves the window hands off to a native OS drag
//     (backend `start_file_drag`) so the file can be dropped into Finder etc.
// Both resolve the target the same way: the row under the point carries
// `data-drop-dir` (a folder → itself, a file → its parent), and the panel body
// carries `data-file-tree-root` (empty space → project root).
import { ref } from 'vue'

export const TREE_ROOT_ATTR = 'data-file-tree-root'
export const DROP_DIR_ATTR = 'data-drop-dir'

export interface TreeDropTarget {
  projectPath: string
  dir: string
}

/** Folder currently highlighted as the drop destination (shared by all rows). */
export const dropHover = ref<TreeDropTarget | null>(null)

/** In-tree drag in progress: what is dragged and where the ghost label sits. */
export const treeDrag = ref<{ projectPath: string; path: string; name: string; isDir: boolean; x: number; y: number } | null>(null)

/** A native drag out of the window is running. The tree ignores the native drop
 *  events it generates when it passes back over the window, so dragging out and
 *  back doesn't copy the file into the project. */
export const outboundDrag = ref(false)

const DRAG_THRESHOLD_PX = 4

export function parentDir(relPath: string): string {
  const i = relPath.lastIndexOf('/')
  return i === -1 ? '' : relPath.slice(0, i)
}

/** The tree drop target under a viewport point (CSS px), or null outside any tree. */
export function treeDropTargetAt(x: number, y: number): TreeDropTarget | null {
  const el = document.elementFromPoint(x, y)
  const root = el?.closest<HTMLElement>(`[${TREE_ROOT_ATTR}]`)
  if (!el || !root) return null
  const row = el.closest<HTMLElement>(`[${DROP_DIR_ATTR}]`)
  return {
    projectPath: root.getAttribute(TREE_ROOT_ATTR) ?? '',
    dir: row?.getAttribute(DROP_DIR_ATTR) ?? '',
  }
}

/**
 * Convert a native drag-drop position to viewport CSS px. On macOS wry reports
 * points (already CSS px) despite Tauri typing it as physical; elsewhere it is
 * physical pixels.
 */
export function nativeDropPoint(pos: { x: number; y: number }): { x: number; y: number } {
  if (/Mac/i.test(navigator.platform)) return { x: pos.x, y: pos.y }
  const r = window.devicePixelRatio || 1
  return { x: pos.x / r, y: pos.y / r }
}

/** Whether moving `src` into `dir` is a real, legal move. */
export function canMoveInto(src: string, dir: string): boolean {
  if (dir === parentDir(src)) return false // already there
  return dir !== src && !dir.startsWith(`${src}/`) // not into itself
}

/**
 * Begin a potential in-tree drag from a row's pointerdown. Nothing happens
 * until the pointer travels past a small threshold, so plain clicks still work.
 * On release over a valid folder `onDrop(dir)` runs; leaving the window ends
 * the in-page drag and calls `onDragOut` (start the native drag). The returned
 * flag tells the row to swallow the click that follows a real drag.
 */
export function startTreeDrag(
  e: PointerEvent,
  source: { projectPath: string; path: string; name: string; isDir: boolean },
  onDrop: (dir: string) => void,
  onDragOut?: () => void,
): { dragged: () => boolean } {
  const startX = e.clientX
  const startY = e.clientY
  let dragging = false

  function update(ev: PointerEvent) {
    const target = treeDropTargetAt(ev.clientX, ev.clientY)
    dropHover.value =
      target && target.projectPath === source.projectPath && canMoveInto(source.path, target.dir)
        ? target
        : null
    treeDrag.value = { ...source, x: ev.clientX, y: ev.clientY }
  }

  function onMove(ev: PointerEvent) {
    if (!dragging) {
      if (Math.hypot(ev.clientX - startX, ev.clientY - startY) < DRAG_THRESHOLD_PX) return
      dragging = true
      document.body.style.cursor = 'grabbing'
    }
    const outside = ev.clientX < 0 || ev.clientY < 0
      || ev.clientX >= window.innerWidth || ev.clientY >= window.innerHeight
    if (outside) { leaveWindow(); return }
    update(ev)
  }

  // Some platforms stop reporting moves at the edge instead of going negative.
  function onOut(ev: MouseEvent) {
    if (!ev.relatedTarget) leaveWindow()
  }

  function leaveWindow() {
    if (!dragging || !onDragOut) return
    cleanup()
    // The native session swallows the pointerup, so there is no click to
    // suppress; report "not dragged" to any listener still waiting for it.
    dragging = false
    onDragOut()
  }

  function cleanup() {
    window.removeEventListener('pointermove', onMove)
    window.removeEventListener('pointerup', onUp)
    window.removeEventListener('keydown', onKey, true)
    document.removeEventListener('mouseout', onOut)
    document.body.style.cursor = ''
    treeDrag.value = null
    dropHover.value = null
  }

  function onUp() {
    const target = dragging ? dropHover.value : null
    cleanup()
    if (target) onDrop(target.dir)
  }

  function onKey(ev: KeyboardEvent) {
    if (ev.key !== 'Escape' || !dragging) return
    ev.stopPropagation()
    cleanup()
  }

  window.addEventListener('pointermove', onMove)
  window.addEventListener('pointerup', onUp)
  window.addEventListener('keydown', onKey, true)
  document.addEventListener('mouseout', onOut)
  return { dragged: () => dragging }
}
