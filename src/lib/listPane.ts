// Which screens have their list pane (the left column of a list-detail screen)
// hidden — persisted per screen (stores/uiLayout). Pure, unit-tested.

/** Screens with a hideable list pane. Settings is deliberately not one. */
export const LIST_PANE_KEYS = ['skills', 'rules', 'mcp', 'servers', 'todos', 'notes', 'slack', 'run'] as const
export type ListPaneKey = (typeof LIST_PANE_KEYS)[number]

export function isListPaneKey(v: unknown): v is ListPaneKey {
  return typeof v === 'string' && (LIST_PANE_KEYS as readonly string[]).includes(v)
}

/** Parse the stored set of hidden panes, dropping anything unknown. */
export function parseHiddenPanes(raw: string | null | undefined): ListPaneKey[] {
  if (!raw) return []
  try {
    const v = JSON.parse(raw)
    return Array.isArray(v) ? [...new Set(v.filter(isListPaneKey))] : []
  } catch {
    return []
  }
}

/**
 * Below this width (px) of its screen, a list-detail screen that opts in
 * (`provideListPane(key, { autoHide: true })`) takes its list out of the
 * layout: a split pane or a narrow window. The list is then one ☰ peek away,
 * and the user's own hide / show choice is left untouched.
 */
export const LIST_PANE_NARROW_BELOW = 640

/**
 * Is a screen of this width too narrow for a docked list? A width of 0 means
 * the screen is not laid out (a background tab): keep the previous answer.
 */
export function narrowFromWidth(width: number, prev: boolean, below = LIST_PANE_NARROW_BELOW): boolean {
  if (!(width > 0)) return prev
  return width < below
}

/**
 * What ⌘B / "toggle the list" does: on a narrow screen the list can only
 * float, so it peeks; otherwise it shows a hidden list or hides a shown one.
 */
export function listPaneToggleAction(s: { hidden: boolean; narrow: boolean }): 'show' | 'hide' | 'peek' {
  if (s.narrow) return 'peek'
  return s.hidden ? 'show' : 'hide'
}
