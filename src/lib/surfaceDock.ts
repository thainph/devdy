// A surface pop-out asking to come back into the main window as a tab.
// The main window listens (lib/surfaceOpen `listenSurfaceRequests`).
import { emit } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'

export const SURFACE_DOCK = 'surface:dock'
/** Main window → every surface pop-out: "dock yourself back as a tab" (screen unplugged). */
export const SURFACE_DOCK_ALL = 'surface:dock-all'

export type DockSurface = 'todos' | 'notes' | 'slack' | 'file' | 'prInbox' | 'mentions' | 'calendar' | 'radar'

export interface SurfaceDockRequest {
  surface: DockSurface
  params?: Record<string, string>
}

/** Hand the surface (and where it was) to the main window, then close this window. */
export async function requestDock(surface: DockSurface, params?: Record<string, string>): Promise<void> {
  try {
    await emit(SURFACE_DOCK, { surface, params } satisfies SurfaceDockRequest)
    await getCurrentWindow().close()
  } catch {
    /* outside the Tauri shell */
  }
}
