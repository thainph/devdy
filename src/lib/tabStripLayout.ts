// Chrome-style overflow of the main window's tab strip (components/WorkspaceTabs):
// tabs share the row at equal widths but never shrink below `min`, so titles
// stay readable; what doesn't fit goes behind a "more" button. The current tab
// always stays on screen, taking the last visible slot if it was hidden.
//
// The strip is laid out in units: a tab is one slot, the split pair (two tabs
// drawn as one merged tab) is two. A unit of `w` slots is `w * width + (w - 1)
// * gap` wide (see `unitWidth`).

export interface TabStripOptions {
  min: number
  max: number
  gap: number
  /** Width of the "more" button, reserved when something overflows. */
  more: number
}

export interface TabStripLayout {
  /** Indices (into the units) of the units shown, in strip order. */
  shown: number[]
  hidden: number[]
  /** Width of one slot (a plain tab). */
  width: number
}

/**
 * `units`: how many units there are (each one slot), or the slot count of each
 * unit in strip order. `current`: the unit to keep on screen.
 */
export function tabStripLayout(
  units: number | readonly number[],
  avail: number,
  current: number,
  o: TabStripOptions,
): TabStripLayout {
  const weights = typeof units === 'number' ? Array.from({ length: units }, () => 1) : units.map((w) => Math.max(1, w))
  const count = weights.length
  const all = Array.from({ length: count }, (_, i) => i)
  const slots = weights.reduce((a, w) => a + w, 0)
  const width = (room: number, k: number) => Math.max(o.min, Math.min(o.max, (room - o.gap * (k - 1)) / k))
  // Not measured yet: show everything.
  if (avail <= 0 || count === 0) return { shown: all, hidden: [], width: o.max }
  if (slots * o.min + o.gap * (slots - 1) <= avail) return { shown: all, hidden: [], width: width(avail, slots) }
  const room = avail - o.more - o.gap
  const cap = Math.max(1, Math.floor((room + o.gap) / (o.min + o.gap)))
  const shown: number[] = []
  let used = 0
  for (const i of all) {
    if (used + weights[i] > cap) break
    shown.push(i)
    used += weights[i]
  }
  if (current >= 0 && current < count && !shown.includes(current)) {
    while (shown.length && used + weights[current] > cap) used -= weights[shown.pop()!]
    shown.push(current)
  }
  // A unit wider than the whole row still shows (squeezed by the caller).
  if (!shown.length && count) shown.push(Math.max(0, Math.min(current, count - 1)))
  const kept = new Set(shown)
  return { shown, hidden: all.filter((i) => !kept.has(i)), width: width(room, cap) }
}

/** Width of a unit of `slots` tabs laid out at `width` per slot. */
export function unitWidth(slots: number, width: number, gap: number): number {
  return slots * width + (slots - 1) * gap
}
