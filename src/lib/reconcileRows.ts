/**
 * Helpers for folding a fresh backend list into an already-reactive one without
 * invalidating everything that reads it. Replacing a reactive array (or every
 * entry of a reactive Map) wholesale re-triggers every computed / component
 * that touches it — the tray, the active-runs dock and the History list — even
 * when nothing changed, which adds up with many concurrent sessions polling.
 */

/** True when both records have the same own keys with `===` values. */
export function shallowEqualRecord(a: object, b: object): boolean {
  if (a === b) return true
  const ka = Object.keys(a)
  if (ka.length !== Object.keys(b).length) return false
  const ra = a as Record<string, unknown>
  const rb = b as Record<string, unknown>
  for (const k of ka) {
    if (!Object.prototype.hasOwnProperty.call(b, k) || ra[k] !== rb[k]) return false
  }
  return true
}

/**
 * Reconcile `next` (fresh data, in the desired order) against `current`.
 *
 * Rows already in `current` are REUSED: when their data changed they are
 * patched in place (`Object.assign`, so a reactive row only triggers the
 * fields that actually differ), otherwise left untouched. Returns the new row
 * array, or `null` when the ids and order are identical to `current` — callers
 * then skip reassigning the array, so nothing that only depends on membership
 * re-runs.
 */
export function reconcileById<T extends { id: string }>(current: readonly T[], next: readonly T[]): T[] | null {
  const byId = new Map<string, T>()
  for (const r of current) byId.set(r.id, r)
  let sameOrder = current.length === next.length
  const out: T[] = new Array(next.length)
  for (let i = 0; i < next.length; i++) {
    const fresh = next[i]
    const existing = byId.get(fresh.id)
    if (existing) {
      if (!shallowEqualRecord(existing, fresh)) Object.assign(existing, fresh)
      out[i] = existing
    } else {
      out[i] = fresh
    }
    if (sameOrder && current[i] !== out[i]) sameOrder = false
  }
  return sameOrder ? null : out
}
