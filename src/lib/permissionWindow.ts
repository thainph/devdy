// Pops the pending Claude permission / question prompt out into a standalone OS
// window so it can be dragged onto a second monitor and answered while the main
// window's chat history stays at full width (no reflow / reading-position jump).
//
// The window loads the same SPA at `index.html?permissionWindow=1`; App.vue
// detects the flag and renders a bare PermissionWindow. There is only ever ONE
// such window (fixed label): it mirrors whichever run is being viewed in the
// main window. Opening it again just focuses the existing one.
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'

const PERMISSION_WINDOW_LABEL = 'permission-prompt'

/** Open (or focus) the standalone permission window; returns its handle. */
export async function openPermissionWindow(): Promise<WebviewWindow> {
  const existing = await WebviewWindow.getByLabel(PERMISSION_WINDOW_LABEL)
  if (existing) {
    try {
      await existing.unminimize()
      await existing.show()
      await existing.setFocus()
    } catch {
      /* window may be mid-teardown */
    }
    return existing
  }

  const win = new WebviewWindow(PERMISSION_WINDOW_LABEL, {
    url: 'index.html?permissionWindow=1',
    title: 'Permission — Devdy',
    width: 480,
    height: 640,
    minWidth: 360,
    minHeight: 320,
    alwaysOnTop: true,
  })
  win.once('tauri://error', (e) => {
    console.error('[permissionWindow] failed to open window', e)
  })
  return win
}

/** Close the permission window if it is open (e.g. on app teardown). */
export async function closePermissionWindow(): Promise<void> {
  try {
    const existing = await WebviewWindow.getByLabel(PERMISSION_WINDOW_LABEL)
    await existing?.close()
  } catch {
    /* already gone */
  }
}
