// Opens THE todo / note LIST window — a standalone OS window that sits BESIDE
// the main window instead of a drawer that covers it.
//
// There is only ever ONE such window (fixed label). Opening it again focuses the
// existing one and switches it to the requested tab/project.
//
// On first open it splits the desktop: the main window keeps the left 2/3 and the
// list takes the right 1/3, both filling the full height of the main window's
// work area. To make room it leaves native fullscreen / un-maximizes first; the
// main window is put back exactly where it was when the list window closes.
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import { emit, listen } from '@tauri-apps/api/event'
import { PhysicalPosition, PhysicalSize } from '@tauri-apps/api/dpi'
import { trackWindowForTray } from '@/lib/tray'
import type { ItemKind } from '@/lib/itemWindow'

const LIST_WINDOW_LABEL = 'item-list'
const MAIN_WINDOW_LABEL = 'main'

/** Sent to the already-open list window to switch its tab / project context. */
export const ITEM_LIST_SET_KIND = 'item-list:set-kind'
/** Emitted by the list window; the main window focuses itself and opens the run. */
export const ITEM_LIST_OPEN_RUN = 'item-list:open-run'
/** Emitted by the list window as it unmounts, so the main layout is restored. */
export const ITEM_LIST_CLOSED = 'item-list:closed'

export interface ItemListSetKind {
  kind: ItemKind
  projectId?: string | null
}

export interface ItemListOpenRun {
  projectId: string
  runId: string
}

/** The fraction of the work area the main window keeps; the list takes the rest. */
const MAIN_FRACTION = 2 / 3

/** Placement for the list window, computed from the main window's work area. */
interface Placement {
  x: number
  y: number
  width: number
  height: number
}

// Main window bounds captured right before the split, so closing the list window
// puts the main window back exactly where it was.
interface MainBounds {
  x: number
  y: number
  width: number
  height: number
  maximized: boolean
}
let savedMainBounds: MainBounds | null = null
let restoreListenerArmed = false

/**
 * Run `trigger` (a maximize / fullscreen toggle) and resolve once the window has
 * settled at its new size — maximize and fullscreen animate, so reading the work
 * area immediately would see a mid-animation size. Falls back to a timeout.
 */
async function waitForResize(win: WebviewWindow, trigger: () => Promise<void>): Promise<void> {
  await new Promise<void>((resolve) => {
    let done = false
    let off: (() => void) | null = null
    const finish = () => {
      if (done) return
      done = true
      off?.()
      resolve()
    }
    win
      .onResized(() => finish())
      .then((unlisten) => {
        off = unlisten
      })
      .catch(() => finish())
    void trigger().catch(() => finish())
    setTimeout(finish, 350)
  })
}

/**
 * Lay out the desktop as a 2/3 (main) + 1/3 (list) split and return where the
 * list window should go.
 *
 * The work area (screen minus the macOS menu bar / dock) is read by maximizing
 * the main window momentarily: when maximized, its outer bounds ARE the work area
 * the OS grants, so there is no menu-bar / dock guesswork. The main window is then
 * shrunk to the left 2/3; the caller places the list window in the right 1/3 once
 * it exists.
 */
async function layoutForSplit(main: WebviewWindow): Promise<Placement> {
  // Remember where the main window was, to restore it on close.
  const prevPos = await main.outerPosition()
  const prevSize = await main.outerSize()
  savedMainBounds = {
    x: prevPos.x,
    y: prevPos.y,
    width: prevSize.width,
    height: prevSize.height,
    maximized: await main.isMaximized(),
  }

  // Native fullscreen owns a whole Space — leave it so a second window can sit
  // beside the main one.
  if (await main.isFullscreen()) {
    await waitForResize(main, () => main.setFullscreen(false))
  }

  // Maximize to let the OS hand us the true work area, then read it.
  if (!(await main.isMaximized())) {
    await waitForResize(main, () => main.maximize())
  }
  const areaPos = await main.outerPosition()
  const areaSize = await main.outerSize()

  const mainW = Math.round(areaSize.width * MAIN_FRACTION)
  const listW = areaSize.width - mainW

  // Back to a normal window, then shrink it to the left 2/3.
  await waitForResize(main, () => main.unmaximize())
  await main.setPosition(new PhysicalPosition(areaPos.x, areaPos.y))
  await main.setSize(new PhysicalSize(mainW, areaSize.height))

  return { x: areaPos.x + mainW, y: areaPos.y, width: listW, height: areaSize.height }
}

/** Put the main window back where it was before the split. */
async function restoreMain(): Promise<void> {
  const bounds = savedMainBounds
  savedMainBounds = null
  if (!bounds) return
  const main = await WebviewWindow.getByLabel(MAIN_WINDOW_LABEL)
  if (!main) return
  try {
    if (bounds.maximized) {
      await main.maximize()
    } else {
      await main.setPosition(new PhysicalPosition(bounds.x, bounds.y))
      await main.setSize(new PhysicalSize(bounds.width, bounds.height))
    }
    await main.setFocus()
  } catch (e) {
    console.error('[itemListWindow] failed to restore main window', e)
  }
}

/** Listen (once) for the list window closing, to reclaim the split space. */
function armRestoreListener(): void {
  if (restoreListenerArmed) return
  restoreListenerArmed = true
  listen(ITEM_LIST_CLOSED, () => {
    void restoreMain()
  }).catch(() => {
    restoreListenerArmed = false
  })
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
      // Re-point the already-open window; don't re-lay out the desktop.
      await emit(ITEM_LIST_SET_KIND, { kind, projectId } satisfies ItemListSetKind)
    } catch {
      /* window may be mid-teardown */
    }
    return existing
  }

  const main = await WebviewWindow.getByLabel(MAIN_WINDOW_LABEL)
  let placement: Placement | null = null
  if (main) {
    try {
      placement = await layoutForSplit(main)
    } catch (e) {
      console.error('[itemListWindow] failed to lay out split', e)
    }
  }
  armRestoreListener()

  const params = new URLSearchParams({ itemListWindow: '1', kind })
  if (projectId) params.set('projectId', projectId)

  const win = new WebviewWindow(LIST_WINDOW_LABEL, {
    url: `index.html?${params.toString()}`,
    title: 'Todos & Notes — Devdy',
    // Starting size; overridden by the split placement below once created.
    width: 480,
    height: 720,
    minWidth: 320,
    minHeight: 420,
  })
  win.once('tauri://error', (e) => {
    console.error('[itemListWindow] failed to open window', e)
  })
  if (placement) {
    win.once('tauri://created', () => {
      void (async () => {
        try {
          await win.setPosition(new PhysicalPosition(placement!.x, placement!.y))
          await win.setSize(new PhysicalSize(placement!.width, placement!.height))
        } catch (e) {
          console.error('[itemListWindow] failed to place window', e)
        }
      })()
    })
  }
  trackWindowForTray(win)
  return win
}
