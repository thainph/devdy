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
