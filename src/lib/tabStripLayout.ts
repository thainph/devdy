// Chrome-style overflow of the main window's tab strip (components/WorkspaceTabs):
// tabs share the row at equal widths but never shrink below `min`, so titles
// stay readable; what doesn't fit goes behind a "more" button. The current tab
// always stays on screen, taking the last visible slot if it was hidden.

export interface TabStripOptions {
  min: number
  max: number
  gap: number
  /** Width of the "more" button, reserved when something overflows. */
  more: number
}

export interface TabStripLayout {
  /** Indices (into the strip) of the tabs shown, in strip order. */
  shown: number[]
  hidden: number[]
  /** Width of every shown tab. */
  width: number
}

export function tabStripLayout(count: number, avail: number, current: number, o: TabStripOptions): TabStripLayout {
  const all = Array.from({ length: count }, (_, i) => i)
  const width = (room: number, k: number) => Math.max(o.min, Math.min(o.max, (room - o.gap * (k - 1)) / k))
  // Not measured yet: show everything.
  if (avail <= 0 || count === 0) return { shown: all, hidden: [], width: o.max }
  if (count * o.min + o.gap * (count - 1) <= avail) return { shown: all, hidden: [], width: width(avail, count) }
  const room = avail - o.more - o.gap
  const cap = Math.max(1, Math.floor((room + o.gap) / (o.min + o.gap)))
  const shown = all.slice(0, cap)
  if (current >= cap) shown[cap - 1] = current
  const kept = new Set(shown)
  return { shown, hidden: all.filter((i) => !kept.has(i)), width: width(room, cap) }
}
