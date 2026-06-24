// Pops the file viewer out into a standalone OS window so a file can be read
// side-by-side with the main app (e.g. dragged onto a second monitor).
//
// The window loads the same SPA at `index.html?fileWindow=1&…`; App.vue detects
// the `fileWindow` flag and renders a bare, chrome-less FileViewer (no sidebar).
// One window per file (deduped by a deterministic label) — opening the same file
// again just focuses the existing window.
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'

// Small deterministic string hash (djb2) → safe, stable window label per file.
function hashLabel(s: string): string {
  let h = 5381
  for (let i = 0; i < s.length; i++) h = ((h << 5) + h + s.charCodeAt(i)) >>> 0
  return `fileviewer-${h.toString(36)}`
}

function basename(path: string): string {
  return path.split('/').pop() || path
}

/**
 * Open (or focus) a standalone window showing `path` from the project at
 * `projectPath`. `line`, when given, is highlighted/scrolled to.
 */
export async function openFileWindow(
  projectPath: string,
  path: string,
  line?: number | null,
): Promise<void> {
  if (!projectPath || !path) return
  const label = hashLabel(`${projectPath}|${path}`)

  // Already open for this file → just bring it to front.
  const existing = await WebviewWindow.getByLabel(label)
  if (existing) {
    try {
      await existing.unminimize()
      await existing.show()
      await existing.setFocus()
    } catch { /* window may be mid-teardown */ }
    return
  }

  const params = new URLSearchParams({
    fileWindow: '1',
    projectPath,
    path,
  })
  if (line != null) params.set('line', String(line))

  const win = new WebviewWindow(label, {
    url: `index.html?${params.toString()}`,
    title: basename(path),
    width: 900,
    height: 820,
    minWidth: 420,
    minHeight: 300,
  })
  win.once('tauri://error', (e) => {
    console.error('[fileWindow] failed to open viewer window', e)
  })
}
