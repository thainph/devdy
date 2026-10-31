import { shallowReactive } from 'vue'
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
 * the language — or any state baked into the menu, like "Hide" vs "Show
 * Sidebar" or whether New Session is available — changes.
 */

/** Event name emitted by the Rust side; must match `MENU_ACTION_EVENT`. */
const MENU_ACTION_EVENT = 'menu://action'

export const IS_MAC = /mac/i.test(navigator.userAgent)

type ItemSpec =
  | { kind: 'item'; id: string; label: string; accelerator?: string; enabled?: boolean }
  | { kind: 'predefined'; role: string; label?: string }
  | { kind: 'submenu'; label: string; items: ItemSpec[] }
  // Replaced in Rust by the open-window list the tray keeps (see tray.rs).
  | { kind: 'windowList' }
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

/**
 * An OS-provided item, labelled in the app language. Without an explicit label
 * macOS titles these in the *system* language, so an app set to Vietnamese on an
 * English Mac would show "Copy" next to "Ghi chú mới…".
 */
function os(role: string, labelKey: string): ItemSpec {
  return { kind: 'predefined', role, label: t(labelKey) }
}

const SEPARATOR: ItemSpec = { kind: 'separator' }

// Both live in the macOS app menu (and Help / File elsewhere), so the Go menu
// leaves them out rather than listing them twice.
const GO_MENU_EXCLUDED = new Set(['/settings', '/about'])

function goMenuItems(): ItemSpec[] {
  const routes = NAV_ROUTES.filter((r) => !GO_MENU_EXCLUDED.has(r.path))
  return routes.flatMap((route, index): ItemSpec[] => [
    // Same grouping as the sidebar dividers.
    ...(index > 0 && route.group !== routes[index - 1].group ? [SEPARATOR] : []),
    { kind: 'item', id: `go.${route.path}`, label: t(route.labelKey) },
  ])
}

function buildSpec(state: MenuState): MenuSpec {
  const submenus: MenuSpec['submenus'] = []
  const settings: ItemSpec = {
    kind: 'item',
    id: 'go./settings',
    label: t('menu.settings'),
    // The one accelerator kept on purpose: ⌘, for Settings is a platform
    // convention, not an app-specific shortcut.
    accelerator: 'CmdOrCtrl+,',
  }
  const about: ItemSpec = { kind: 'item', id: 'go./about', label: t('menu.about') }

  // macOS puts app-level items (about / settings / services / hide / quit) in a
  // first submenu named after the app. On Windows/Linux that submenu doesn't
  // exist: Settings and Quit ride along in File, About in Help.
  if (IS_MAC) {
    submenus.push({
      label: 'Devdy',
      items: [
        // The app's own About page rather than the bare OS panel, so there's
        // one About, not two.
        about,
        SEPARATOR,
        settings,
        SEPARATOR,
        os('services', 'menu.services'),
        SEPARATOR,
        os('hide', 'menu.hide'),
        os('hideOthers', 'menu.hideOthers'),
        os('showAll', 'menu.showAll'),
        SEPARATOR,
        os('quit', 'menu.quit'),
      ],
    })
  }

  submenus.push({
    label: t('menu.file'),
    items: [
      {
        kind: 'item',
        id: 'file.newSession',
        label: t('menu.newSession'),
        // Only the project screen can start a session (it needs the project);
        // greyed out elsewhere instead of silently doing nothing.
        enabled: isMenuActionBound('file.newSession'),
      },
      { kind: 'item', id: 'file.newTodo', label: t('menu.newTodo') },
      { kind: 'item', id: 'file.newNote', label: t('menu.newNote') },
      SEPARATOR,
      ...(IS_MAC
        ? [os('closeWindow', 'menu.closeWindow')]
        : [settings, SEPARATOR, os('closeWindow', 'menu.closeWindow'), os('quit', 'menu.quit')]),
    ],
  })

  // Not optional: on macOS the system only delivers ⌘C/⌘V/⌘Z to the webview
  // when the menu bar actually contains those items. Dropping this submenu
  // silently kills clipboard shortcuts across the whole app.
  submenus.push({
    label: t('menu.edit'),
    items: [
      os('undo', 'menu.undo'),
      os('redo', 'menu.redo'),
      SEPARATOR,
      os('cut', 'menu.cut'),
      os('copy', 'menu.copy'),
      os('paste', 'menu.paste'),
      os('selectAll', 'menu.selectAll'),
    ],
  })

  // Layout toggles only; things that open a window live under Window.
  submenus.push({
    label: t('menu.view'),
    items: [
      {
        kind: 'item',
        id: 'view.toggleSidebar',
        label: state.sidebarHidden ? t('menu.showSidebar') : t('menu.hideSidebar'),
      },
      {
        kind: 'item',
        id: 'view.toggleFocus',
        label: state.focusMode ? t('menu.exitFocusMode') : t('menu.enterFocusMode'),
      },
      SEPARATOR,
      // The OS full-screen item only exists on macOS; elsewhere it's a no-op,
      // so the window is toggled from the frontend instead.
      IS_MAC
        ? os('fullscreen', 'menu.fullscreen')
        : { kind: 'item', id: 'view.toggleFullscreen', label: t('menu.fullscreen') },
    ],
  })

  submenus.push({ label: t('menu.go'), items: goMenuItems() })

  submenus.push({
    label: t('menu.window'),
    items: [
      os('minimize', 'menu.minimize'),
      os('maximize', IS_MAC ? 'menu.zoom' : 'menu.maximize'),
      SEPARATOR,
      { kind: 'item', id: 'window.todoList', label: t('menu.todoList') },
      { kind: 'item', id: 'window.noteList', label: t('menu.noteList') },
      { kind: 'item', id: 'window.secretNotes', label: t('menu.secretNotes') },
      { kind: 'item', id: 'window.slackThreads', label: t('menu.slackThreads') },
      SEPARATOR,
      { kind: 'windowList' },
      ...(IS_MAC ? [SEPARATOR, os('bringAllToFront', 'menu.bringAllToFront')] : []),
    ],
  })

  submenus.push({
    label: t('menu.help'),
    items: [
      { kind: 'item', id: 'help.docs', label: t('menu.docs') },
      { kind: 'item', id: 'help.reportIssue', label: t('menu.reportIssue') },
      { kind: 'item', id: 'help.openLogs', label: t('menu.openLogs') },
      SEPARATOR,
      // Reload drops all in-memory UI state, so it's tucked away here rather
      // than sitting in View where it's easy to hit by mistake.
      {
        kind: 'submenu',
        label: t('menu.developer'),
        items: [{ kind: 'item', id: 'help.reload', label: t('menu.reload') }],
      },
      ...(IS_MAC ? [] : [SEPARATOR, about]),
    ],
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

// Reactive so the menu can grey out an item whose screen isn't mounted (see
// `isMenuActionBound`).
const handlers = shallowReactive(new Map<string, MenuHandler>())

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

/** Whether some live component currently handles `id`. Reactive. */
export function isMenuActionBound(id: string): boolean {
  return handlers.has(id)
}

function runMenuAction(id: string): void {
  handlers.get(id)?.()
}

/** Subscribe to menu clicks coming from Rust. Returns an unlisten function. */
export async function listenMenuActions(): Promise<UnlistenFn> {
  return listen<string>(MENU_ACTION_EVENT, (event) => {
    if (typeof event.payload === 'string') runMenuAction(event.payload)
  })
}
