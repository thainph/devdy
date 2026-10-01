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
import {
  getAllWebviewWindows,
  getCurrentWebviewWindow,
  type WebviewWindow,
} from '@tauri-apps/api/webviewWindow'
import { invoke } from '@/lib/tauri'
import { i18n } from '@/i18n'
import { useLiveRunsStore } from '@/stores/liveRuns'

type TrayItem = { kind: 'item'; id: string; label: string } | { kind: 'separator' }

/**
 * Keep the menu-bar switcher in sync as a pop-out comes and goes: call this right
 * after `new WebviewWindow(...)` so the window appears in the switcher once it's
 * created and drops out when it's closed. Any window the user can switch to should
 * be tracked (session, file viewer, gantt, item editor, …).
 */
export function trackWindowForTray(win: WebviewWindow): void {
  win.once('tauri://created', () => {
    void refreshTray()
  })
  win.once('tauri://destroyed', () => {
    void refreshTray()
  })
}

// Menu ids share a `tray:` namespace so the Rust app-menu handler ignores them
// (both handlers see every click — see tray.rs). `tray:win:<label>` focuses a
// window; `tray:quit` quits.
const QUIT_ID = 'tray:quit'
const winId = (label: string) => `tray:win:${label}`

function t(key: string): string {
  return i18n.global.t(key)
}

// Windows that are never useful "switch to" targets: the always-on desktop pet
// and the transient permission modal (it pops in and auto-dismisses, so listing
// it would just make a row flicker). Everything else is listed.
const HIDDEN_WINDOW_LABELS = new Set(['mascot', 'permission-prompt'])

// The label shown for a window in the switcher. Some window kinds carry a
// genuinely meaningful OS title — a session (the run name), a file viewer (the
// file's basename) and an item editor (the note title / todo id) — so those use
// the live title, falling back to a generic name. Every other kind gets a fixed,
// localized name keyed off its label prefix; their OS titles are just static
// placeholders like "Gantt — Devdy".
function labelFor(windowLabel: string, osTitle: string): string {
  const title = osTitle.trim()
  const meaningful = title && title !== 'Devdy' ? title : ''

  if (windowLabel === 'main') return t('tray.mainWindow')
  if (windowLabel.startsWith('session-')) return meaningful || t('tray.untitledSession')
  if (windowLabel.startsWith('fileviewer-')) return meaningful || t('tray.fileWindow')
  if (windowLabel === 'gantt') return t('tray.ganttWindow')
  if (windowLabel === 'item-create') return t('tray.newItem')
  if (windowLabel.startsWith('item-edit-')) return meaningful || t('tray.editItem')
  // Unknown / future window kind: best effort — its title, else the raw label.
  return meaningful || windowLabel
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

    // Main window pinned first; every other Devdy window follows (except the
    // hidden ones above), sorted by label so the order stays stable across
    // refreshes rather than following getAllWebviewWindows' arbitrary order.
    if (wins.some((w) => w.label === 'main')) {
      items.push({ kind: 'item', id: winId('main'), label: t('tray.mainWindow') })
    }

    const others = wins
      .filter((w) => w.label !== 'main' && !HIDDEN_WINDOW_LABELS.has(w.label))
      .sort((a, b) => a.label.localeCompare(b.label))
    for (const w of others) {
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

    // The badge = every running session (including ones never popped out into
    // their own window), so it must come from the main window's store — each
    // pop-out only tracks its own run. Other windows omit `title` entirely so
    // they can't clobber the count the main window set (see tray.rs).
    const spec: { items: TrayItem[]; title?: string } = { items }
    let isMain = false
    try {
      isMain = getCurrentWebviewWindow().label === 'main'
    } catch {
      /* outside the Tauri shell */
    }
    if (isMain) {
      const runningCount = useLiveRunsStore().runningIds.length
      spec.title = runningCount > 0 ? String(runningCount) : ''
    }

    await invoke('set_tray_menu', { spec })
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
