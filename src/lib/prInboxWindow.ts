// Opens THE PR Inbox window (views/PrInboxWindow.vue, `?prInboxWindow=1`): the
// PR review inbox on its own OS window, so it can sit beside a running session
// or on a second monitor. There is only ever ONE such window (fixed label);
// opening it again focuses it. Mirrors lib/slackWindow.ts.
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import { trackWindowForTray } from '@/lib/tray'
import { centerOnCurrentMonitor } from '@/lib/windowCascade'

/** Window label; src-tauri/capabilities/default.json grants it the app permissions. */
export const PR_INBOX_WINDOW_LABEL = 'pr-inbox'

/**
 * Emitted by the PR Inbox window; the main window focuses itself and navigates
 * (the pop-out has no router of the app) — e.g. to /projects for "Add to
 * devdy". Handled in App.vue. Opening a review run goes through
 * WINDOW_OPEN_RUN of lib/sessionLinks instead, like the Slack window.
 */
export const PR_INBOX_WINDOW_OPEN_ROUTE = 'pr-inbox-window:open-route'

export interface PrInboxWindowOpenRoute {
  path: string
}

// In-flight creation guard (like lib/mascotWindow.ts): two quick clicks / a
// click plus the ⌘2 shortcut would otherwise both pass the getByLabel() null
// check before either window exists, and create 'pr-inbox' twice.
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

export async function openPrInboxWindow(): Promise<WebviewWindow | null> {
  // A creation started by another caller: wait for it, then just focus.
  if (creating) {
    const win = await creating
    await focusWindow(win)
    return win
  }
  const existing = await WebviewWindow.getByLabel(PR_INBOX_WINDOW_LABEL)
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

  creating = createPrInboxWindow()
  try {
    return await creating
  } finally {
    creating = null
  }
}

async function createPrInboxWindow(): Promise<WebviewWindow> {
  const width = 1100
  const height = 780
  const win = new WebviewWindow(PR_INBOX_WINDOW_LABEL, {
    url: 'index.html?prInboxWindow=1',
    title: 'PR Inbox — Devdy',
    width,
    height,
    minWidth: 720,
    minHeight: 480,
    ...(await centerOnCurrentMonitor(width, height)),
  })
  win.once('tauri://error', (e) => {
    console.error('[prInboxWindow] failed to open window', e)
  })
  trackWindowForTray(win)
  return win
}
