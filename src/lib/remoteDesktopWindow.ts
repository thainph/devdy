// Opens THE remote-desktop window (`?remote=1`): the normal Devdy UI, but its
// invoke/listen drive ANOTHER Devdy over the relay (see lib/ipc/remote.ts and
// the remote boot in main.ts). There is only ever ONE such window (fixed
// label); opening it again focuses it. Mirrors lib/prInboxWindow.ts.
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import { trackWindowForTray } from '@/lib/tray'
import { centerOnCurrentMonitor } from '@/lib/windowCascade'

/** Window label; src-tauri/capabilities/default.json grants it the app permissions. */
export const REMOTE_DESKTOP_WINDOW_LABEL = 'remote-desktop'

// In-flight creation guard (like lib/prInboxWindow.ts): two quick clicks would
// otherwise both pass the getByLabel() null check and create the window twice.
let creating: Promise<WebviewWindow> | null = null

async function focusWindow(win: WebviewWindow) {
  try {
    await win.unminimize()
    await win.show()
    await win.setFocus()
  } catch {
    /* window may be mid-teardown */
  }
}

export async function openRemoteDesktopWindow(): Promise<WebviewWindow | null> {
  // A creation started by another caller: wait for it, then just focus.
  if (creating) {
    const win = await creating
    await focusWindow(win)
    return win
  }
  const existing = await WebviewWindow.getByLabel(REMOTE_DESKTOP_WINDOW_LABEL)
  if (existing) {
    await focusWindow(existing)
    return existing
  }
  // Re-check: another caller may have started creating while we awaited.
  if (creating) {
    const win = await creating
    await focusWindow(win)
    return win
  }

  creating = createRemoteDesktopWindow()
  try {
    return await creating
  } finally {
    creating = null
  }
}

async function createRemoteDesktopWindow(): Promise<WebviewWindow> {
  const width = 1280
  const height = 860
  const win = new WebviewWindow(REMOTE_DESKTOP_WINDOW_LABEL, {
    url: 'index.html?remote=1',
    title: 'Remote Devdy',
    width,
    height,
    minWidth: 900,
    minHeight: 600,
    ...(await centerOnCurrentMonitor(width, height)),
  })
  win.once('tauri://error', (e) => {
    console.error('[remoteDesktopWindow] failed to open window', e)
  })
  trackWindowForTray(win)
  return win
}
