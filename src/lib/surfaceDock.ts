// "Dock into main window": a pop-out surface (Todos / Notes, Slack threads, a
// file, PR Inbox, Mentions, Calendar, Delivery Radar) asks the main window to
// reopen it as a tab, at the same place (`params`: selected id, project, file
// path…), then closes itself. The main window listens for SURFACE_DOCK.
import { emit } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'

export const SURFACE_DOCK = 'surface:dock'

export type DockSurface = 'todos' | 'notes' | 'slack' | 'file' | 'prInbox' | 'mentions' | 'calendar' | 'radar'

export interface SurfaceDockRequest {
  surface: DockSurface
  /** What reopens it at the same place; only the set values. */
  params?: Record<string, string>
}

/** Hand this pop-out's surface to the main window, then close the pop-out. */
export async function requestDock(surface: DockSurface, params?: Record<string, string>): Promise<void> {
  await emit(SURFACE_DOCK, { surface, params } satisfies SurfaceDockRequest)
  try {
    await getCurrentWindow().close()
  } catch {
    /* already closing */
  }
}
