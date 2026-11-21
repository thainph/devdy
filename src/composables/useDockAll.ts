// A surface pop-out answers the main window's "dock everything" (the second
// screen was unplugged, lib/surfaceOpen `dockAllSurfaceWindows`) by running its
// own dock action. Not for embedded (tab) instances: they are docked already.
import { onBeforeUnmount, onMounted } from 'vue'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { SURFACE_DOCK_ALL } from '@/lib/surfaceDock'

export function useDockAll(dock: () => unknown, enabled = true) {
  if (!enabled) return
  let unlisten: UnlistenFn | null = null
  onMounted(async () => {
    try {
      unlisten = await listen(SURFACE_DOCK_ALL, () => void dock())
    } catch {
      /* outside the Tauri shell */
    }
  })
  onBeforeUnmount(() => unlisten?.())
}
