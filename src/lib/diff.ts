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
  // dp[i][j] = LCS length of a[i:] and b[j:].
  const dp: number[][] = Array.from({ length: n + 1 }, () => new Array(m + 1).fill(0))
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      dp[i][j] = a[i] === b[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1])
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
    } else if (dp[i + 1][j] >= dp[i][j + 1]) {
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
