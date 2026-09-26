// macOS menu-bar (system tray) window switcher.
//
// The tray icon is created in Rust (see src-tauri/src/commands/tray.rs); its menu
// is described here and pushed down via `set_tray_menu`, mirroring how the app
// menu bar works (appMenu.ts). That keeps labels on the app language and lets each
// session pop-out show its own run title.
//
// The menu lists every open window: the main window first, then each pop-out
// (session, gantt, file viewer, item editor). Clicking a row focuses that window;
// Rust maps the item id (the window label) back to the window. The mascot pet is
// skipped — it isn't a window you'd "switch to".
//
// Re-sent whenever the window set changes: on startup, when a session window opens
// or closes, and when a run is renamed (which updates the pop-out's OS title).
import { getAllWebviewWindows } from '@tauri-apps/api/webviewWindow'
import { invoke } from '@/lib/tauri'
import { i18n } from '@/i18n'

type TrayItem = { kind: 'item'; id: string; label: string } | { kind: 'separator' }

// Menu ids share a `tray:` namespace so the Rust app-menu handler ignores them
// (both handlers see every click — see tray.rs). `tray:win:<label>` focuses a
// window; `tray:quit` quits.
const QUIT_ID = 'tray:quit'
const winId = (label: string) => `tray:win:${label}`

function t(key: string): string {
  return i18n.global.t(key)
}

// A window title of "Devdy" is the default placeholder set in tauri.conf.json /
// the pop-out builders — not a meaningful name — so fall back to the label then.
function labelFor(windowLabel: string, osTitle: string): string {
  const title = osTitle.trim()
  if (title && title !== 'Devdy') return title
  if (windowLabel.startsWith('session-')) return t('tray.untitledSession')
  return windowLabel
}

// Serialize refreshes: several triggers (open + title-set + destroyed) can fire in
// a burst, and set_tray_menu should apply them in order without overlapping.
let refreshing = false
let queued = false

/** Rebuild the tray menu from the currently open windows and push it to Rust. */
export async function refreshTray(): Promise<void> {
  if (refreshing) {
    queued = true
    return
  }
  refreshing = true
  try {
    const wins = await getAllWebviewWindows()
    const items: TrayItem[] = []

    if (wins.some((w) => w.label === 'main')) {
      items.push({ kind: 'item', id: winId('main'), label: t('tray.mainWindow') })
    }

    // Only the run pop-outs are meaningful "switch to" targets. The transient
    // editors (file viewer / item / permission) and the desktop-pet mascot are
    // deliberately left out to keep the menu a clean session switcher.
    for (const w of wins) {
      if (!w.label.startsWith('session-')) continue
      let title = ''
      try {
        title = await w.title()
      } catch {
        /* window may be mid-teardown */
      }
      items.push({ kind: 'item', id: winId(w.label), label: labelFor(w.label, title) })
    }

    items.push({ kind: 'separator' })
    items.push({ kind: 'item', id: QUIT_ID, label: t('tray.quit') })

    await invoke('set_tray_menu', { spec: { items } })
  } catch {
    // Outside the Tauri shell (browser dev) there is no tray.
  } finally {
    refreshing = false
    if (queued) {
      queued = false
      void refreshTray()
    }
  }
}
