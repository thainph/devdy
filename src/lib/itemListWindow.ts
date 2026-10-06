// Opens THE todo / note LIST window — a standalone OS window docked against the
// right edge of the monitor the user is working on (same as the permission
// window), so it reads like a side panel beside the main window WITHOUT ever
// resizing it (resizing the main window was janky).
//
// There is only ever ONE such window (fixed label). Opening it again focuses the
// existing one and switches it to the requested tab/project. App.vue detects the
// `itemListWindow` flag and renders a bare ItemListWindow.
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import { emit } from '@tauri-apps/api/event'
import { trackWindowForTray } from '@/lib/tray'
import { dockRightOnCurrentMonitor } from '@/lib/windowCascade'
import type { ItemKind } from '@/lib/itemWindow'

const LIST_WINDOW_LABEL = 'item-list'
const LIST_WINDOW_WIDTH = 460
// Cap the panel height so it doesn't stretch the full height of a large external
// monitor; it stays a comfortable side-panel size and scrolls if the list is long.
const LIST_WINDOW_MAX_HEIGHT = 860

/** Sent to the already-open list window to switch its tab / project context. */
export const ITEM_LIST_SET_KIND = 'item-list:set-kind'
/** Emitted by the list window; the main window focuses itself and opens the run. */
export const ITEM_LIST_OPEN_RUN = 'item-list:open-run'

export interface ItemListSetKind {
  kind: ItemKind
  projectId?: string | null
}

export interface ItemListOpenRun {
  projectId: string
  runId: string
}

/**
 * Open (or focus) the standalone todo / note list window, on the given tab and
 * optionally scoped to a project.
 */
export async function openItemListWindow(
  kind: ItemKind = 'todo',
  opts: { projectId?: string | null } = {},
): Promise<WebviewWindow | null> {
  const projectId = opts.projectId ?? null

  const existing = await WebviewWindow.getByLabel(LIST_WINDOW_LABEL)
  if (existing) {
    try {
      await existing.unminimize()
      await existing.show()
      await existing.setFocus()
      // Re-point the already-open window.
      await emit(ITEM_LIST_SET_KIND, { kind, projectId } satisfies ItemListSetKind)
    } catch {
      /* window may be mid-teardown */
    }
    return existing
  }

  // Dock against the right edge of the monitor the user is working on, spanning its
  // height — a side panel beside the main window, which is never resized.
  const dock = await dockRightOnCurrentMonitor(
    LIST_WINDOW_WIDTH,
    24,
    LIST_WINDOW_MAX_HEIGHT,
  )
  const params = new URLSearchParams({ itemListWindow: '1', kind })
  if (projectId) params.set('projectId', projectId)

  const win = new WebviewWindow(LIST_WINDOW_LABEL, {
    url: `index.html?${params.toString()}`,
    title: 'Todos & Notes — Devdy',
    width: LIST_WINDOW_WIDTH,
    height: dock?.height ?? 720,
    minWidth: 320,
    minHeight: 420,
    ...(dock ? { x: dock.x, y: dock.y } : {}),
  })
  win.once('tauri://error', (e) => {
    console.error('[itemListWindow] failed to open window', e)
  })
  trackWindowForTray(win)
  return win
}
