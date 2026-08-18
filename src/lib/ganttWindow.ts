// Opens the Gantt chart in a standalone OS window so it can live on a second
// monitor (or beside the app) while the main window stays fully usable.
//
// The window loads the same SPA at `index.html?ganttWindow=1&project=…`;
// App.vue detects the flag and renders a bare IssuesGanttView. There is only
// ever ONE such window (fixed label): opening it again focuses the existing one
// and switches it to the requested project.
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import { emit } from '@tauri-apps/api/event'

const GANTT_WINDOW_LABEL = 'gantt'

/** Open (or focus) the standalone Gantt window, optionally on a given project. */
export async function openGanttWindow(projectId?: string): Promise<WebviewWindow> {
  const existing = await WebviewWindow.getByLabel(GANTT_WINDOW_LABEL)
  if (existing) {
    try {
      await existing.unminimize()
      await existing.show()
      await existing.setFocus()
      // Tell the already-open window to switch to the requested project.
      if (projectId) await emit('gantt:set-project', { projectId })
    } catch {
      /* window may be mid-teardown */
    }
    return existing
  }

  const query = projectId ? `&project=${encodeURIComponent(projectId)}` : ''
  const win = new WebviewWindow(GANTT_WINDOW_LABEL, {
    url: `index.html?ganttWindow=1${query}`,
    title: 'Gantt — Devdy',
    width: 1200,
    height: 760,
    minWidth: 720,
    minHeight: 480,
  })
  win.once('tauri://error', (e) => {
    console.error('[ganttWindow] failed to open window', e)
  })
  return win
}
