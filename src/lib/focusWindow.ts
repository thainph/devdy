// Opens THE Focus window (views/FocusWindow.vue, `?focusWindow=1`): the Focus
// board on its own OS window, so it can sit beside a running session or
// on a second monitor. There is only ever ONE such window (fixed label);
// opening it again focuses it. Mirrors lib/prInboxWindow.ts.
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import { trackWindowForTray } from '@/lib/tray'
import { centerOnCurrentMonitor } from '@/lib/windowCascade'
import { popoutLabel, popoutUrl } from '@/lib/popout'

/** Window label; src-tauri/capabilities/default.json grants it the app permissions. */
export const FOCUS_WINDOW_LABEL = 'focus'

// In-flight creation guard (like lib/prInboxWindow.ts): two quick clicks would
// otherwise both pass the getByLabel() null check and create 'focus' twice.
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

export async function openFocusWindow(): Promise<WebviewWindow | null> {
  // A creation started by another caller: wait for it, then just focus.
  if (creating) {
    const win = await creating
    await focusWindow(win)
    return win
  }
  const existing = await WebviewWindow.getByLabel(popoutLabel(FOCUS_WINDOW_LABEL))
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

  creating = createFocusWindow()
  try {
    return await creating
  } finally {
    creating = null
  }
}

async function createFocusWindow(): Promise<WebviewWindow> {
  const width = 1100
  const height = 780
  const win = new WebviewWindow(popoutLabel(FOCUS_WINDOW_LABEL), {
    url: popoutUrl('index.html?focusWindow=1'),
    title: 'Focus — Devdy',
    width,
    height,
    minWidth: 720,
    minHeight: 480,
    ...(await centerOnCurrentMonitor(width, height)),
  })
  win.once('tauri://error', (e) => {
    console.error('[focusWindow] failed to open window', e)
  })
  trackWindowForTray(win)
  return win
}
