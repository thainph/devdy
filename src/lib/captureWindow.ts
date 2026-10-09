// Opens the detail window for one capture — a Slack thread or a web page pushed
// in by the Chrome extension (views/CaptureWindow.vue, `?captureWindow=1&id=…`).
// It is the capture analog of the note editor window (lib/itemWindow). One
// window PER capture (deterministic label), so opening the same capture twice
// focuses the window already showing it.
//
// It is a sibling of the item window rather than another kind inside it: a
// capture is never created by hand (it arrives from the Inbox API), and its
// detail carries source metadata and attachments the todo / note editor has no
// place for.
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import { trackWindowForTray } from '@/lib/tray'
import { nextCascadePosition } from '@/lib/windowCascade'
import type { CaptureKind } from '@/stores/captures'

/**
 * Window label prefix; lib/tray.ts names these rows in the menu-bar switcher and
 * src-tauri/capabilities/default.json grants them the app permissions.
 */
export const CAPTURE_WINDOW_PREFIX = 'capture-'

/**
 * Emitted by the backend when the user clicks the OS notification for a capture
 * received over the Inbox API (it also focuses the main window). Only the main
 * window listens (App.vue), so the detail window opens exactly once.
 */
export const CAPTURE_NOTIFICATION_CLICKED = 'capture-notification-clicked'

export interface CaptureNotificationClicked {
  id: string
  kind: CaptureKind
}

// Small deterministic string hash (djb2) → safe, stable window label per capture.
function labelFor(id: string): string {
  let h = 5381
  for (let i = 0; i < id.length; i++) h = ((h << 5) + h + id.charCodeAt(i)) >>> 0
  return `${CAPTURE_WINDOW_PREFIX}${h.toString(36)}`
}

/**
 * Open (or focus) the detail window for one capture. `kind` only picks the
 * window's wording before the capture loads (or if it no longer exists).
 */
export async function openCaptureWindow(
  id: string,
  kind?: CaptureKind,
): Promise<WebviewWindow | null> {
  if (!id) return null
  const label = labelFor(id)

  const existing = await WebviewWindow.getByLabel(label)
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

  const params = new URLSearchParams({ captureWindow: '1', id })
  if (kind) params.set('kind', kind)
  const win = new WebviewWindow(label, {
    url: `index.html?${params.toString()}`,
    title: 'Capture — Devdy',
    width: 720,
    height: 800,
    minWidth: 420,
    minHeight: 420,
    // Opens focused, so start on top; the window lowers itself on blur unless
    // pinned (see useFloatingWindow).
    alwaysOnTop: true,
    ...(await nextCascadePosition()),
  })
  win.once('tauri://error', (e) => {
    console.error('[captureWindow] failed to open window', e)
  })
  trackWindowForTray(win)
  return win
}
