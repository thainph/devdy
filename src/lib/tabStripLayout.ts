// Chrome-style overflow of the main window's tab strip (components/WorkspaceTabs):
// tabs share the row at equal widths but never shrink below `min`, so titles
// stay readable; what doesn't fit goes behind a "more" button. The current tab
// always stays on screen, taking the last visible slot if it was hidden — and
// so does every other tab passed to keep (split view: both halves' tabs).
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
 * unit in strip order. `current`: the unit to keep on screen, or several (most
 * important first; a later one gives way when they don't all fit).
 */
export function tabStripLayout(
  units: number | readonly number[],
  avail: number,
  current: number | readonly number[],
  o: TabStripOptions,
): TabStripLayout {
  const keep = (typeof current === 'number' ? [current] : [...current])
    .filter((i, n, list) => Number.isInteger(i) && i >= 0 && list.indexOf(i) === n)
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
  // Bring each kept unit back, dropping the last shown units that aren't kept
  // (never an earlier kept one) until it fits.
  const placed: number[] = []
  for (const k of keep) {
    if (k >= count) continue
    if (!shown.includes(k)) {
      while (used + weights[k] > cap) {
        const drop = shown.map((_, j) => j).reverse().find((j) => !placed.includes(shown[j]))
        if (drop === undefined) break
        used -= weights[shown.splice(drop, 1)[0]]
      }
      if (used + weights[k] > cap && placed.length) continue
      shown.push(k)
      used += weights[k]
    }
    placed.push(k)
  }
  // A unit wider than the whole row still shows (squeezed by the caller).
  if (!shown.length && count) shown.push(Math.max(0, Math.min(keep[0] ?? 0, count - 1)))
  const kept = new Set(shown)
  return { shown, hidden: all.filter((i) => !kept.has(i)), width: width(room, cap) }
}

/** Width of a unit of `slots` tabs laid out at `width` per slot. */
export function unitWidth(slots: number, width: number, gap: number): number {
  return slots * width + (slots - 1) * gap
}
