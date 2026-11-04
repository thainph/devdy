// Pops a run's pending Claude permission / question prompt out into its OWN
// standalone OS window, docked against the right edge of the monitor the user is
// working on — so it reads like a side panel and never covers the chat, and the
// main window is never resized.
//
// There is ONE window PER run (deterministic label), so several runs asking at
// once — the main window, a popped-out session, or a conductor's workers — each
// get their own window answering their own run. The window loads the same SPA at
// `index.html?permissionWindow=1&runId=…`; App.vue detects the flag and renders a
// bare PermissionWindow that mirrors that run's head request. The global
// PermissionWindowManager owns the open/close lifecycle and the resolve logic.
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import { trackWindowForTray } from '@/lib/tray'
import { dockRightOnCurrentMonitor } from '@/lib/windowCascade'

const PERMISSION_WINDOW_WIDTH = 460
// Cap the panel height so it doesn't stretch the full height of a large external
// monitor — the prompt content is short, so a side panel this tall is plenty.
const PERMISSION_WINDOW_MAX_HEIGHT = 720
// Gap between stacked permission windows when more than one is open at once.
const STACK_GAP = 12

// Small deterministic string hash (djb2) → a stable, label-safe window id per run.
function hashLabel(s: string): string {
  let h = 5381
  for (let i = 0; i < s.length; i++) h = ((h << 5) + h + s.charCodeAt(i)) >>> 0
  return `permission-${h.toString(36)}`
}

/** The window label for a run's permission prompt. */
export function permissionWindowLabel(runId: string): string {
  return hashLabel(runId)
}

/**
 * Open (or focus) the permission window for one run.
 *
 * `indexFromRight` stacks multiple permission windows leftward from the right edge
 * so they sit side by side instead of on top of each other.
 */
export async function openPermissionWindow(
  runId: string,
  indexFromRight = 0,
): Promise<WebviewWindow> {
  const label = permissionWindowLabel(runId)

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

  // Dock against the right edge of the monitor the user is working on, spanning
  // its height — a side panel beside the main window, which is never resized.
  const dock = await dockRightOnCurrentMonitor(
    PERMISSION_WINDOW_WIDTH,
    24,
    PERMISSION_WINDOW_MAX_HEIGHT,
  )
  const shift = indexFromRight * (PERMISSION_WINDOW_WIDTH + STACK_GAP)
  const params = new URLSearchParams({ permissionWindow: '1', runId })
  const win = new WebviewWindow(label, {
    url: `index.html?${params.toString()}`,
    title: 'Permission — Devdy',
    width: PERMISSION_WINDOW_WIDTH,
    height: dock?.height ?? 640,
    minWidth: 360,
    minHeight: 320,
    alwaysOnTop: true,
    ...(dock ? { x: dock.x - shift, y: dock.y } : {}),
  })
  win.once('tauri://error', (e) => {
    console.error('[permissionWindow] failed to open window', e)
  })
  trackWindowForTray(win)
  return win
}

/** Close the permission window for a run if it is open. */
export async function closePermissionWindow(runId: string): Promise<void> {
  try {
    const existing = await WebviewWindow.getByLabel(permissionWindowLabel(runId))
    await existing?.close()
  } catch {
    /* already gone */
  }
}
