import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { invoke } from '@/lib/tauri'
import { i18n } from '@/i18n'
import { NAV_ROUTES } from '@/lib/navigation'

/**
 * Native menu bar (File / Edit / View / Go / Window / Help).
 *
 * Tauri ships a default menu with nothing app-specific in it. This module
 * replaces it: the spec below is sent to the `set_app_menu` command, and Rust
 * reports clicks back on `menu://action` carrying the item id. Actions are
 * resolved HERE, in the frontend, because that's where the router, the layout
 * store and the quick-capture overlay live.
 *
 * Labels are built from vue-i18n, so `applyAppMenu()` must be re-run whenever
 * the language — or any state baked into a label, like "Hide" vs "Show
 * Sidebar" — changes.
 */

/** Event name emitted by the Rust side; must match `MENU_ACTION_EVENT`. */
const MENU_ACTION_EVENT = 'menu://action'

export const IS_MAC = /mac/i.test(navigator.userAgent)

type ItemSpec =
  | { kind: 'item'; id: string; label: string; accelerator?: string }
  | { kind: 'predefined'; role: string; label?: string }
  | { kind: 'separator' }

interface MenuSpec {
  submenus: { label: string; items: ItemSpec[] }[]
}

export interface MenuState {
  sidebarHidden: boolean
  focusMode: boolean
}

function t(key: string): string {
  return i18n.global.t(key)
}

function buildSpec(state: MenuState): MenuSpec {
  const submenus: MenuSpec['submenus'] = []

  // macOS puts app-level items (about / services / hide / quit) in a first
  // submenu named after the app. On Windows/Linux those live elsewhere, so the
  // submenu is skipped entirely and Quit rides along in File.
  if (IS_MAC) {
    submenus.push({
      label: 'Devdy',
      items: [
        { kind: 'predefined', role: 'about' },
        { kind: 'separator' },
        { kind: 'item', id: 'go./settings', label: t('menu.settings'), accelerator: 'CmdOrCtrl+,' },
        { kind: 'separator' },
        { kind: 'predefined', role: 'services' },
        { kind: 'separator' },
        { kind: 'predefined', role: 'hide' },
        { kind: 'predefined', role: 'hideOthers' },
        { kind: 'predefined', role: 'showAll' },
        { kind: 'separator' },
        { kind: 'predefined', role: 'quit' },
      ],
    })
  }

  submenus.push({
    label: t('menu.file'),
    items: [
      { kind: 'item', id: 'file.newSession', label: t('menu.newSession'), accelerator: 'CmdOrCtrl+N' },
      { kind: 'item', id: 'file.newTodo', label: t('menu.newTodo'), accelerator: 'CmdOrCtrl+K' },
      { kind: 'item', id: 'file.newNote', label: t('menu.newNote'), accelerator: 'CmdOrCtrl+Shift+N' },
      { kind: 'separator' },
      ...(IS_MAC
        ? ([{ kind: 'predefined', role: 'closeWindow' }] as ItemSpec[])
        : ([
            { kind: 'predefined', role: 'closeWindow' },
            { kind: 'separator' },
            { kind: 'predefined', role: 'quit' },
          ] as ItemSpec[])),
    ],
  })

  // Not optional: on macOS the system only delivers ⌘C/⌘V/⌘Z to the webview
  // when the menu bar actually contains those items. Dropping this submenu
  // silently kills clipboard shortcuts across the whole app.
  submenus.push({
    label: t('menu.edit'),
    items: [
      { kind: 'predefined', role: 'undo' },
      { kind: 'predefined', role: 'redo' },
      { kind: 'separator' },
      { kind: 'predefined', role: 'cut' },
      { kind: 'predefined', role: 'copy' },
      { kind: 'predefined', role: 'paste' },
      { kind: 'predefined', role: 'selectAll' },
    ],
  })

  submenus.push({
    label: t('menu.view'),
    items: [
      {
        kind: 'item',
        id: 'view.toggleSidebar',
        label: state.sidebarHidden ? t('menu.showSidebar') : t('menu.hideSidebar'),
        accelerator: 'CmdOrCtrl+B',
      },
      {
        kind: 'item',
        id: 'view.itemPanel',
        label: t('menu.itemPanel'),
        accelerator: 'CmdOrCtrl+Shift+K',
      },
      {
        kind: 'item',
        id: 'view.toggleFocus',
        label: state.focusMode ? t('menu.exitFocusMode') : t('menu.enterFocusMode'),
        accelerator: 'CmdOrCtrl+Shift+F',
      },
      { kind: 'separator' },
      { kind: 'item', id: 'view.reload', label: t('menu.reload'), accelerator: 'CmdOrCtrl+R' },
      { kind: 'predefined', role: 'fullscreen' },
    ],
  })

  submenus.push({
    label: t('menu.go'),
    items: NAV_ROUTES.map((route, index) => ({
      kind: 'item' as const,
      id: `go.${route.path}`,
      label: t(route.labelKey),
      // ⌘1…⌘9 for the first nine destinations; the rest stay click-only rather
      // than inventing awkward two-key bindings.
      accelerator: index < 9 ? `CmdOrCtrl+${index + 1}` : undefined,
    })),
  })

  submenus.push({
    label: t('menu.window'),
    items: [
      { kind: 'predefined', role: 'minimize' },
      { kind: 'predefined', role: 'maximize' },
      { kind: 'separator' },
      { kind: 'predefined', role: 'closeWindow' },
    ],
  })

  submenus.push({
    label: t('menu.help'),
    items: [{ kind: 'item', id: 'go./about', label: t('menu.about') }],
  })

  return { submenus }
}

/** Install (or re-install) the menu for the given layout state. */
export async function applyAppMenu(state: MenuState): Promise<void> {
  try {
    await invoke('set_app_menu', { spec: buildSpec(state) })
  } catch {
    // Outside the Tauri shell (browser dev) there is no menu bar to set.
  }
}

// ── Action dispatch ─────────────────────────────────────────────────────────

type MenuHandler = () => void

const handlers = new Map<string, MenuHandler>()
const lastRun = new Map<string, number>()

/**
 * Bind an action id to a handler for as long as a component is alive. Returns
 * the unbind function. Used for actions only one screen can perform — e.g. only
 * RunView can start a new session.
 */
export function registerMenuAction(id: string, handler: MenuHandler): () => void {
  handlers.set(id, handler)
  return () => {
    if (handlers.get(id) === handler) handlers.delete(id)
  }
}

/**
 * Run an action, ignoring a repeat within 250ms.
 *
 * Several of these ids have BOTH a menu accelerator and an in-webview keydown
 * binding (⌘K / ⌘⇧N predate the menu, ⌘B has to keep working in pop-out windows
 * that carry no menu). Whether the OS swallows the key before the webview sees
 * it is platform- and focus-dependent, so the same press can arrive twice —
 * which for a toggle like the sidebar would cancel itself out and look dead.
 */
export function runMenuAction(id: string): void {
  const now = Date.now()
  const previous = lastRun.get(id)
  if (previous !== undefined && now - previous < 250) return
  lastRun.set(id, now)
  handlers.get(id)?.()
}

/** Subscribe to menu clicks coming from Rust. Returns an unlisten function. */
export async function listenMenuActions(): Promise<UnlistenFn> {
  return listen<string>(MENU_ACTION_EVENT, (event) => {
    if (typeof event.payload === 'string') runMenuAction(event.payload)
  })
}
