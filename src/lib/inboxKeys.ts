// Keyboard navigation of the inbox lists (PR Inbox, GitHub Mentions):
// j / k or ↓ / ↑ move the selection, Enter runs the row's primary action,
// o opens it on GitHub, c copies its link.

export type InboxKeyAction = 'next' | 'prev' | 'primary' | 'open' | 'copy'

/** The action for a key press, or null when the list should ignore it. */
export function inboxKeyAction(key: string, mods: { meta?: boolean; ctrl?: boolean; alt?: boolean } = {}): InboxKeyAction | null {
  if (mods.meta || mods.ctrl || mods.alt) return null
  switch (key) {
    case 'j': case 'ArrowDown': return 'next'
    case 'k': case 'ArrowUp': return 'prev'
    case 'Enter': return 'primary'
    case 'o': return 'open'
    case 'c': return 'copy'
    default: return null
  }
}

/**
 * The selection after moving `dir` rows through `keys` (visible order). No
 * selection, or one that is no longer listed, starts at the first row (down)
 * or the last (up); the ends clamp rather than wrap.
 */
export function moveSelection(keys: readonly string[], current: string | null, dir: 1 | -1): string | null {
  if (keys.length === 0) return null
  const i = current === null ? -1 : keys.indexOf(current)
  if (i === -1) return dir === 1 ? keys[0] : keys[keys.length - 1]
  return keys[Math.min(keys.length - 1, Math.max(0, i + dir))]
}
