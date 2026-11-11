// Line-level diff (LCS) used to render Edit/MultiEdit/Write tool calls as a
// unified, GitHub-style diff. Kept dependency-free and small: inputs are code
// snippets (Edit old/new strings) or a single file body (Write), not whole
// repos, so the O(n·m) table is fine — with a hard cap that falls back to a
// plain block diff for pathologically large inputs.

export type DiffRow =
  | { type: 'ctx' | 'add' | 'del'; text: string; oldNo?: number; newNo?: number }
  | { type: 'fold'; count: number }

export interface DiffResult {
  rows: DiffRow[]
  added: number
  removed: number
}

// Guard: skip the DP table when before×after line counts get huge.
const MAX_CELLS = 4_000_000

function lcsDiff(a: string[], b: string[]): DiffRow[] {
  const n = a.length
  const m = b.length
  // dp[i*w + j] = LCS length of a[i:] and b[j:]. One flat typed array instead
  // of n+1 boxed-number rows: a fraction of the memory and no per-row GC churn.
  const w = m + 1
  const dp = new Uint32Array((n + 1) * w)
  for (let i = n - 1; i >= 0; i--) {
    const row = i * w
    const next = row + w
    for (let j = m - 1; j >= 0; j--) {
      dp[row + j] = a[i] === b[j] ? dp[next + j + 1] + 1 : Math.max(dp[next + j], dp[row + j + 1])
    }
  }
  const rows: DiffRow[] = []
  let i = 0
  let j = 0
  let oldNo = 1
  let newNo = 1
  while (i < n && j < m) {
    if (a[i] === b[j]) {
      rows.push({ type: 'ctx', text: a[i], oldNo: oldNo++, newNo: newNo++ })
      i++
      j++
    } else if (dp[(i + 1) * w + j] >= dp[i * w + j + 1]) {
      rows.push({ type: 'del', text: a[i], oldNo: oldNo++ })
      i++
    } else {
      rows.push({ type: 'add', text: b[j], newNo: newNo++ })
      j++
    }
  }
  while (i < n) {
    rows.push({ type: 'del', text: a[i], oldNo: oldNo++ })
    i++
  }
  while (j < m) {
    rows.push({ type: 'add', text: b[j], newNo: newNo++ })
    j++
  }
  return rows
}

// Collapse long runs of unchanged lines, keeping `context` lines of padding
// around each change so the reader still sees where a hunk sits.
function foldRows(rows: DiffRow[], context: number): DiffRow[] {
  const keep = new Array(rows.length).fill(false)
  rows.forEach((r, idx) => {
    if (r.type !== 'ctx') {
      const from = Math.max(0, idx - context)
      const to = Math.min(rows.length - 1, idx + context)
      for (let k = from; k <= to; k++) keep[k] = true
    }
  })
  const out: DiffRow[] = []
  let i = 0
  while (i < rows.length) {
    if (keep[i]) {
      out.push(rows[i])
      i++
    } else {
      let j = i
      while (j < rows.length && !keep[j]) j++
      out.push({ type: 'fold', count: j - i })
      i = j
    }
  }
  return out
}

function splitLines(s: string): string[] {
  if (!s) return []
  // Drop a single trailing newline so a file ending in "\n" doesn't render a
  // spurious blank final row.
  const t = s.endsWith('\n') ? s.slice(0, -1) : s
  return t.split('\n')
}

/**
 * Just the +added / −removed counts `computeDiff` would report, without
 * building rows. Every LCS alignment has the same length, so the counts are
 * `n − L` / `m − L`; the common head and tail are always part of an LCS, so
 * they are trimmed first (an Edit's old/new strings mostly share both), and L
 * of the middle takes two rolling rows instead of a full table. Same cap as
 * `computeDiff`, so a header badge always matches the diff it summarizes.
 */
export function diffStats(before: string, after: string): { added: number; removed: number } {
  const a = splitLines(before)
  const b = splitLines(after)
  const n = a.length
  const m = b.length
  if (!n || !m || n * m > MAX_CELLS) return { added: m, removed: n }
  let lo = 0
  while (lo < n && lo < m && a[lo] === b[lo]) lo++
  let hiA = n
  let hiB = m
  while (hiA > lo && hiB > lo && a[hiA - 1] === b[hiB - 1]) {
    hiA--
    hiB--
  }
  let prev = new Uint32Array(hiB - lo + 1)
  let cur = new Uint32Array(hiB - lo + 1)
  for (let i = hiA - 1; i >= lo; i--) {
    for (let j = hiB - 1; j >= lo; j--) {
      const k = j - lo
      cur[k] = a[i] === b[j] ? prev[k + 1] + 1 : Math.max(prev[k], cur[k + 1])
    }
    const t = prev
    prev = cur
    cur = t
  }
  const lcs = lo + (n - hiA) + prev[0]
  return { added: m - lcs, removed: n - lcs }
}

export function computeDiff(
  before: string,
  after: string,
  opts: { context?: number; fold?: boolean } = {},
): DiffResult {
  const a = splitLines(before)
  const b = splitLines(after)
  let rows: DiffRow[]
  if (!a.length) {
    rows = b.map((text, k) => ({ type: 'add' as const, text, newNo: k + 1 }))
  } else if (!b.length) {
    rows = a.map((text, k) => ({ type: 'del' as const, text, oldNo: k + 1 }))
  } else if (a.length * b.length > MAX_CELLS) {
    rows = [
      ...a.map((text, k) => ({ type: 'del' as const, text, oldNo: k + 1 })),
      ...b.map((text, k) => ({ type: 'add' as const, text, newNo: k + 1 })),
    ]
  } else {
    rows = lcsDiff(a, b)
  }
  const added = rows.reduce((n, r) => (r.type === 'add' ? n + 1 : n), 0)
  const removed = rows.reduce((n, r) => (r.type === 'del' ? n + 1 : n), 0)
  const display = opts.fold === false ? rows : foldRows(rows, opts.context ?? 3)
  return { rows: display, added, removed }
}
