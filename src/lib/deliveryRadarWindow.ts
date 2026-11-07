// Opens the Delivery Radar (who is late on their GitHub issues, across
// projects) in a standalone OS window so it can live on a second monitor (or
// beside the app) while the main window stays fully usable.
//
// The window loads the same SPA at `index.html?radarWindow=1&project=…`;
// App.vue detects the flag and renders a bare DeliveryRadarView. There is only
// ever ONE such window (fixed label): opening it again focuses the existing one
// and, when a project is given, narrows it to that project.
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import { emit } from '@tauri-apps/api/event'
import { trackWindowForTray } from '@/lib/tray'
import { centerOnCurrentMonitor } from '@/lib/windowCascade'

export const DELIVERY_RADAR_WINDOW_LABEL = 'delivery-radar'

/**
 * Emitted by the radar window; the main window focuses itself and navigates
 * (the pop-out has no router of the app) — e.g. to a project's settings to map
 * its Deadline field. Handled in App.vue.
 */
export const DELIVERY_RADAR_OPEN_ROUTE = 'delivery-radar-window:open-route'

export interface DeliveryRadarOpenRoute {
  path: string
}

/** Open (or focus) the Delivery Radar window, optionally narrowed to one project. */
export async function openDeliveryRadarWindow(projectId?: string): Promise<WebviewWindow> {
  const existing = await WebviewWindow.getByLabel(DELIVERY_RADAR_WINDOW_LABEL)
  if (existing) {
    try {
      await existing.unminimize()
      await existing.show()
      await existing.setFocus()
      // Tell the already-open window to narrow to the requested project.
      if (projectId) await emit('radar:set-project', { projectId })
    } catch {
      /* window may be mid-teardown */
    }
    return existing
  }

  const query = projectId ? `&project=${encodeURIComponent(projectId)}` : ''
  const win = new WebviewWindow(DELIVERY_RADAR_WINDOW_LABEL, {
    url: `index.html?radarWindow=1${query}`,
    title: 'Delivery Radar — Devdy',
    width: 1200,
    height: 760,
    minWidth: 720,
    minHeight: 480,
    // Open on the monitor the user is working on, not the primary screen.
    ...(await centerOnCurrentMonitor(1200, 760)),
  })
  win.once('tauri://error', (e) => {
    console.error('[deliveryRadarWindow] failed to open window', e)
  })
  trackWindowForTray(win)
  return win
}
