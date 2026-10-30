// Menu-bar badge + tray row of the Focus board (docs/focus-board-spec.md §3.4).
// Pure, so it can be unit-tested (src/lib/__tests__/focusTray.test.mjs).

/** Tray row id; Rust (commands/tray.rs) opens the Focus window on a click. */
export const TRAY_FOCUS_ID = 'tray:focus'

/**
 * Text beside the menu-bar glyph: the running-session count, then `⏰N` for
 * overdue todos (`''` = icon only). E.g. `2`, `⏰3`, `2 ⏰3`.
 */
export function trayBadge(running: number, overdue: number): string {
  const parts: string[] = []
  if (running > 0) parts.push(String(running))
  if (overdue > 0) parts.push(`⏰${overdue}`)
  return parts.join(' ')
}
