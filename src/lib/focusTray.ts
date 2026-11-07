// Menu-bar badge + tray row of the Focus board (docs/focus-board-spec.md §3.4).
// Pure, so it can be unit-tested (src/lib/__tests__/focusTray.test.mjs).

/** Tray row id; Rust (commands/tray.rs) opens the Todos window on a click. */
export const TRAY_FOCUS_ID = 'tray:focus'

/**
 * Text beside the menu-bar glyph: the running-session count (`''` = icon only).
 * Overdue todos are only shown on the tray's Focus row, not beside the glyph.
 */
export function trayBadge(running: number): string {
  return running > 0 ? String(running) : ''
}
