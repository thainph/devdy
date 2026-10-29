// Opens THE Pulse window (views/PulseWindow.vue, `?pulseWindow=1`): the Project
// Pulse screen on its own OS window, so it can sit beside a running session or
// on a second monitor. There is only ever ONE such window (fixed label);
// opening it again focuses it. Mirrors lib/prInboxWindow.ts.
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import { trackWindowForTray } from '@/lib/tray'
import { centerOnCurrentMonitor } from '@/lib/windowCascade'
import { popoutLabel, popoutUrl } from '@/lib/popout'

/** Window label; src-tauri/capabilities/default.json grants it the app permissions. */
export const PULSE_WINDOW_LABEL = 'pulse'

// In-flight creation guard (like lib/prInboxWindow.ts): two quick clicks would
// otherwise both pass the getByLabel() null check and create 'pulse' twice.
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

export async function openPulseWindow(): Promise<WebviewWindow | null> {
  // A creation started by another caller: wait for it, then just focus.
  if (creating) {
    const win = await creating
    await focusWindow(win)
    return win
  }
  const existing = await WebviewWindow.getByLabel(popoutLabel(PULSE_WINDOW_LABEL))
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

  creating = createPulseWindow()
  try {
    return await creating
  } finally {
    creating = null
  }
}

async function createPulseWindow(): Promise<WebviewWindow> {
  const width = 1100
  const height = 780
  const win = new WebviewWindow(popoutLabel(PULSE_WINDOW_LABEL), {
    url: popoutUrl('index.html?pulseWindow=1'),
    title: 'Pulse — Devdy',
    width,
    height,
    minWidth: 720,
    minHeight: 480,
    ...(await centerOnCurrentMonitor(width, height)),
  })
  win.once('tauri://error', (e) => {
    console.error('[pulseWindow] failed to open window', e)
  })
  trackWindowForTray(win)
  return win
}
