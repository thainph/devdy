// Devdy's keyboard shortcuts, as listed on the About screen. Keys are written
// as platform-neutral tokens (`Mod` = ⌘ on macOS, Ctrl elsewhere) and turned
// into labels by `keyLabel`. Keep in step with the native menu's accelerators
// (lib/appMenu) and the in-app handlers (composer, editors, file viewer).

export type ShortcutKey =
  | 'Mod'
  | 'Shift'
  | 'Esc'
  | 'Enter'
  | 'Tab'
  | 'Up'
  | 'Down'
  | string

export interface Shortcut {
  /** i18n key under `about.shortcuts.items`. */
  id: string
  /** One or more alternative key combos ("or"). */
  keys: ShortcutKey[][]
  /** Windows / Linux keys when they differ beyond Mod = Ctrl. */
  otherKeys?: ShortcutKey[][]
  /** macOS only (e.g. ⌘Q is the system's on other platforms). */
  macOnly?: boolean
}

export interface ShortcutGroup {
  /** i18n key under `about.shortcuts.groups`. */
  id: string
  items: Shortcut[]
}

export const SHORTCUT_GROUPS: ShortcutGroup[] = [
  {
    id: 'general',
    items: [
      { id: 'palette', keys: [['Mod', 'K']] },
      { id: 'quickOpen', keys: [['Mod', 'P']] },
      { id: 'settings', keys: [['Mod', ',']] },
      { id: 'shortcuts', keys: [['Mod', '/']] },
      { id: 'quit', keys: [['Mod', 'Q']], macOnly: true },
    ],
  },
  {
    id: 'tabs',
    items: [
      { id: 'gotoTab', keys: [['Mod', '1…8']] },
      { id: 'lastTab', keys: [['Mod', '9']] },
      { id: 'prevNextTab', keys: [['Mod', 'Shift', '['], ['Mod', 'Shift', ']']] },
      { id: 'closeTab', keys: [['Mod', 'Shift', 'W']] },
      { id: 'split', keys: [['Mod', '\\']] },
      { id: 'closeWindow', keys: [['Mod', 'W']] },
    ],
  },
  {
    id: 'layout',
    items: [
      { id: 'toggleSidebar', keys: [['Ctrl', 'Mod', 'S']], otherKeys: [['Ctrl', 'Alt', 'S']] },
      { id: 'toggleList', keys: [['Mod', 'B']] },
      { id: 'focus', keys: [['Mod', '.']] },
      { id: 'escape', keys: [['Esc']] },
    ],
  },
  {
    id: 'create',
    items: [
      { id: 'newSession', keys: [['Mod', 'N']] },
      { id: 'newTodo', keys: [['Mod', 'Alt', 'N']] },
    ],
  },
  {
    id: 'session',
    items: [
      { id: 'send', keys: [['Enter']] },
      { id: 'newline', keys: [['Shift', 'Enter']] },
      { id: 'slash', keys: [['/']] },
      { id: 'mention', keys: [['@']] },
      { id: 'pick', keys: [['Up'], ['Down'], ['Enter'], ['Tab']] },
    ],
  },
  {
    id: 'editing',
    items: [
      { id: 'save', keys: [['Mod', 'Enter']] },
      { id: 'cancel', keys: [['Esc']] },
      { id: 'find', keys: [['Mod', 'F']] },
    ],
  },
]

const MAC: Record<string, string> = {
  Mod: '⌘', Shift: '⇧', Alt: '⌥', Ctrl: '⌃', Esc: 'Esc', Enter: '↩', Tab: '⇥', Up: '↑', Down: '↓',
}
const OTHER: Record<string, string> = {
  Mod: 'Ctrl', Shift: 'Shift', Alt: 'Alt', Ctrl: 'Ctrl', Esc: 'Esc', Enter: 'Enter', Tab: 'Tab', Up: '↑', Down: '↓',
}

/** How a key token reads on this platform. */
export function keyLabel(key: ShortcutKey, mac: boolean): string {
  return (mac ? MAC : OTHER)[key] ?? key
}

/** The groups shown on this platform: mac-only rows dropped, platform keys picked. */
export function shortcutGroupsFor(mac: boolean): ShortcutGroup[] {
  return SHORTCUT_GROUPS.map((g) => ({
    ...g,
    items: g.items
      .filter((s) => mac || !s.macOnly)
      .map((s) => (!mac && s.otherKeys ? { ...s, keys: s.otherKeys } : s)),
  }))
}
