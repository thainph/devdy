// Opens ONE session (run) in its own standalone OS window, so several
// conversations can be watched/driven side by side while the main window stays
// on whatever it was showing.
//
// The window loads the same SPA at `index.html?sessionWindow=1&projectId=…&runId=…`;
// App.vue detects the flag, hides the app chrome (sidebar / nav / docks /
// notifiers) and routes straight to that run — so it reuses the full RunView
// without any duplication.
//
// There is only ever ONE window PER run (deterministic label): popping the same
// session again — or clicking the same worker in a conductor's sidebar — focuses
// the window already showing it rather than opening a rival copy.
//
// The run itself lives in the backend registry keyed by run_id, not in any
// webview, so closing this window merely detaches its listeners: the sidecar
// keeps streaming and the main window (or another popout) stays in sync via the
// per-run events. Closing the MAIN window still tears every popout down and kills
// the sidecars (see src-tauri/src/lib.rs).
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import { trackWindowForTray } from '@/lib/tray'

// Small deterministic string hash (djb2) → a stable, label-safe window id per run.
function hashLabel(s: string): string {
  let h = 5381
  for (let i = 0; i < s.length; i++) h = ((h << 5) + h + s.charCodeAt(i)) >>> 0
  return `session-${h.toString(36)}`
}

/** Bring an existing window to the front. Returns false if it's mid-teardown. */
async function focusWindow(win: WebviewWindow): Promise<boolean> {
  try {
    await win.unminimize()
    await win.show()
    await win.setFocus()
    return true
  } catch {
    return false
  }
}

/**
 * Open (or focus) the standalone window for one run. Safe to call repeatedly for
 * the same run: the second call just focuses the first window.
 */
export async function openSessionWindow(
  projectId: string,
  runId: string,
): Promise<WebviewWindow | null> {
  if (!projectId || !runId) return null
  const label = hashLabel(runId)

  const existing = await WebviewWindow.getByLabel(label)
  if (existing) {
    await focusWindow(existing)
    return existing
  }

  const params = new URLSearchParams({ sessionWindow: '1', projectId, runId })
  const win = new WebviewWindow(label, {
    url: `index.html?${params.toString()}`,
    title: 'Session — Devdy',
    width: 1000,
    height: 720,
    minWidth: 600,
    minHeight: 420,
  })
  win.once('tauri://error', (e) => {
    console.error('[sessionWindow] failed to open window', e)
  })
  // Keep the menu-bar switcher in sync as this window comes and goes. The window
  // also sets its own OS title from the run name (App.vue), which refreshes the
  // tray again once that lands — this just covers the open/close edges.
  trackWindowForTray(win)
  return win
}
