// Opens THE todo / note window — the one and only place an item is written.
//
// Creating and editing both happen in a standalone, always-on-top OS window:
// never a drawer, never a modal, never a screen. That is what makes the app
// consistent about these two objects — wherever a todo or a note is touched, it
// is the same window, and the main window never has to give up what it was
// showing to let one be written.
//
// Two modes, one window component (views/ItemWindow.vue):
//   • create — one shared window (fixed label), Todo/Note tabs, keeps taking
//     captures until it's closed. Opening it again focuses it and switches tab.
//   • edit   — one window PER item (deterministic label), so popping the same
//     note twice focuses the window already editing it rather than opening a
//     rival editor for the same row.
//
// A pre-filled capture ("save this selection as a note") has to travel to a
// window that may not exist yet, so it goes as an event behind a ready
// handshake: a freshly spawned webview isn't listening, and Tauri events are not
// buffered.
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import { emit, listen } from '@tauri-apps/api/event'

export type ItemKind = 'todo' | 'note'
export type ItemWindowMode = 'create' | 'edit'

/** Project / run the capture should be filed under, when opened from one. */
export interface ItemContext {
  projectId?: string | null
  runId?: string | null
}

/** Text to start a capture with (e.g. the selected run output). */
export interface ItemPrefill {
  title?: string
  content?: string
}

/** Sent by the create window when reopened: switch tab + re-point the context. */
export interface ItemWindowSetTab extends ItemContext {
  kind: ItemKind
}

const CREATE_WINDOW_LABEL = 'item-create'

export const ITEM_WINDOW_READY = 'item:window-ready'
export const ITEM_WINDOW_SET_TAB = 'item:window-set-tab'
export const ITEM_WINDOW_PREFILL = 'item:window-prefill'

/**
 * Broadcast after the window writes anything (create, save, delete, toggle), so
 * the list drawer in the main window — a different webview, with its own store —
 * refetches instead of showing stale rows. Payload: `{ kind }`.
 */
export const ITEM_CHANGED = 'item:changed'

// Generous: the window has to boot the SPA before it can listen. On timeout we
// send the prefill anyway; losing it is better than hanging the caller.
const READY_TIMEOUT_MS = 6000

// Small deterministic string hash (djb2) → safe, stable window label per item.
function hashLabel(s: string): string {
  let h = 5381
  for (let i = 0; i < s.length; i++) h = ((h << 5) + h + s.charCodeAt(i)) >>> 0
  return `item-edit-${h.toString(36)}`
}

function hasPrefill(prefill?: ItemPrefill): boolean {
  return !!(prefill?.title?.trim() || prefill?.content?.trim())
}

/** Bring an existing window to the front. Returns false if it's mid-teardown. */
async function focusWindow(win: WebviewWindow): Promise<boolean> {
  try {
    await win.unminimize()
    await win.show()
    await win.setFocus()
    return true
  } catch {
    return false
  }
}

/**
 * Wait for the window to say it is listening.
 *
 * Must be armed BEFORE the window is created, otherwise a fast start-up can ping
 * before we are listening and the prefill misses its slot.
 */
async function waitForReady(): Promise<() => Promise<void>> {
  let signal: () => void = () => {}
  const ready = new Promise<void>((resolve) => {
    signal = resolve
  })
  const unlisten = await listen(ITEM_WINDOW_READY, () => signal())
  const timer = setTimeout(signal, READY_TIMEOUT_MS)
  return async () => {
    await ready
    clearTimeout(timer)
    unlisten()
  }
}

/**
 * Open (or focus) the shared "new todo / note" window on the given tab.
 *
 * `prefill` starts the capture with text already in it; it only ever fills
 * fields that are still empty in the window, so it cannot clobber a capture the
 * user is already typing there.
 */
export async function openItemCreateWindow(
  kind: ItemKind = 'todo',
  context: ItemContext = {},
  prefill?: ItemPrefill,
): Promise<WebviewWindow> {
  const payload: ItemWindowSetTab = {
    kind,
    projectId: context.projectId ?? null,
    runId: context.runId ?? null,
  }

  const existing = await WebviewWindow.getByLabel(CREATE_WINDOW_LABEL)
  if (existing) {
    if (await focusWindow(existing)) {
      // Already listening, so no handshake is needed.
      await emit(ITEM_WINDOW_SET_TAB, payload)
      if (hasPrefill(prefill)) await emit(ITEM_WINDOW_PREFILL, prefill)
    }
    return existing
  }

  const params = new URLSearchParams({ itemWindow: '1', mode: 'create', kind })
  if (context.projectId) params.set('projectId', context.projectId)
  if (context.runId) params.set('runId', context.runId)

  const awaitReady = hasPrefill(prefill) ? await waitForReady() : null

  const win = new WebviewWindow(CREATE_WINDOW_LABEL, {
    url: `index.html?${params.toString()}`,
    title: 'New — Devdy',
    width: 420,
    height: 600,
    minWidth: 340,
    minHeight: 380,
    alwaysOnTop: true,
  })
  win.once('tauri://error', (e) => {
    console.error('[itemWindow] failed to open create window', e)
  })

  if (awaitReady) {
    await awaitReady()
    await emit(ITEM_WINDOW_PREFILL, prefill)
  }
  return win
}

/** Open (or focus) the editor window for one existing todo / note. */
export async function openItemEditWindow(
  kind: ItemKind,
  itemId: string,
): Promise<WebviewWindow | null> {
  if (!itemId) return null
  const label = hashLabel(`${kind}|${itemId}`)

  const existing = await WebviewWindow.getByLabel(label)
  if (existing) {
    await focusWindow(existing)
    return existing
  }

  const params = new URLSearchParams({ itemWindow: '1', mode: 'edit', kind, id: itemId })
  const win = new WebviewWindow(label, {
    url: `index.html?${params.toString()}`,
    title: kind === 'note' ? 'Note — Devdy' : 'Task — Devdy',
    width: 520,
    height: 620,
    minWidth: 360,
    minHeight: 320,
    alwaysOnTop: true,
  })
  win.once('tauri://error', (e) => {
    console.error('[itemWindow] failed to open edit window', e)
  })
  return win
}
