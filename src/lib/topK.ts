/**
 * The `k` highest-scoring items, best first, without sorting the whole input.
 * Items scoring below 0 are dropped. Ties keep source order — the same result
 * as a stable `filter(s >= 0).sort(desc).slice(0, k)`, but O(n log k) with a
 * k-sized buffer instead of allocating and sorting every match (the @-mention
 * picker scores up to ~50k project paths per query).
 */
export function topKByScore<T>(items: readonly T[], score: (item: T) => number, k: number): T[] {
  if (k <= 0) return []
  const buf: Array<{ item: T; s: number }> = []
  for (const item of items) {
    const s = score(item)
    if (s < 0) continue
    // Full and not strictly better than the worst kept: an equal score loses
    // to the earlier item already in the buffer (stable order).
    if (buf.length === k && s <= buf[k - 1].s) continue
    // First index whose score is strictly lower → insert there (after equals).
    let lo = 0
    let hi = buf.length
    while (lo < hi) {
      const mid = (lo + hi) >> 1
      if (buf[mid].s >= s) lo = mid + 1
      else hi = mid
    }
    buf.splice(lo, 0, { item, s })
    if (buf.length > k) buf.pop()
  }
  return buf.map((x) => x.item)
}
