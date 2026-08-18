// Opens the "quick create" Todo/Note form in a standalone, always-on-top OS
// window so it can live on a second monitor (or beside the app) and be filled
// in while the main window stays fully usable — no overlay, no reflow.
//
// The window loads the same SPA at `index.html?quickCreateWindow=1&tab=…`;
// App.vue detects the flag and renders a bare QuickCreateWindow. There is only
// ever ONE such window (fixed label): opening it again focuses the existing one
// and just switches its active tab.
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import { emit } from '@tauri-apps/api/event'

export type QuickCreateTab = 'todo' | 'note'

const QUICK_CREATE_WINDOW_LABEL = 'quick-create'

/** Open (or focus) the standalone quick-create window on the given tab. */
export async function openQuickCreateWindow(tab: QuickCreateTab = 'todo'): Promise<WebviewWindow> {
  const existing = await WebviewWindow.getByLabel(QUICK_CREATE_WINDOW_LABEL)
  if (existing) {
    try {
      await existing.unminimize()
      await existing.show()
      await existing.setFocus()
      // Tell the already-open window to switch to the requested tab.
      await emit('quickcreate:set-tab', { tab })
    } catch {
      /* window may be mid-teardown */
    }
    return existing
  }

  const win = new WebviewWindow(QUICK_CREATE_WINDOW_LABEL, {
    url: `index.html?quickCreateWindow=1&tab=${tab}`,
    title: 'Quick Create — Devdy',
    width: 380,
    height: 520,
    minWidth: 320,
    minHeight: 360,
    alwaysOnTop: true,
  })
  win.once('tauri://error', (e) => {
    console.error('[quickCreateWindow] failed to open window', e)
  })
  return win
}
