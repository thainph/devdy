// Opens THE GitHub Mentions window (views/GithubMentionsWindow.vue,
// `?githubMentionsWindow=1`): the mentions list on its own OS window, beside a
// running session or on a second monitor. There is only ever ONE such window
// (fixed label); opening it again focuses it. Mirrors lib/prInboxWindow.ts.
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import { trackWindowForTray } from '@/lib/tray'
import { centerOnCurrentMonitor } from '@/lib/windowCascade'

/** Window label; src-tauri/capabilities/default.json grants it the app permissions. */
export const GITHUB_MENTIONS_WINDOW_LABEL = 'github-mentions'

// In-flight creation guard: two quick clicks would otherwise both pass the
// getByLabel() null check and create the window twice.
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

export async function openGithubMentionsWindow(): Promise<WebviewWindow | null> {
  if (creating) {
    const win = await creating
    await focusWindow(win)
    return win
  }
  const existing = await WebviewWindow.getByLabel(GITHUB_MENTIONS_WINDOW_LABEL)
  if (existing) {
    await focusWindow(existing)
    return existing
  }
  if (creating) {
    const win = await creating
    await focusWindow(win)
    return win
  }
  creating = createWindow()
  try {
    return await creating
  } finally {
    creating = null
  }
}

async function createWindow(): Promise<WebviewWindow> {
  const width = 1000
  const height = 760
  const win = new WebviewWindow(GITHUB_MENTIONS_WINDOW_LABEL, {
    url: 'index.html?githubMentionsWindow=1',
    title: 'GitHub Mentions — Devdy',
    width,
    height,
    minWidth: 680,
    minHeight: 460,
    ...(await centerOnCurrentMonitor(width, height)),
  })
  win.once('tauri://error', (e) => {
    console.error('[githubMentionsWindow] failed to open window', e)
  })
  trackWindowForTray(win)
  return win
}
