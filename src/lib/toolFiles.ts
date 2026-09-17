// Which file(s) a tool call touches, across engines. Claude tools carry a single
// path key (`file_path` / `notebook_path` / `path`); Codex maps its `fileChange`
// item and `apply_patch` call onto the same `Edit` tool name but keeps its own
// payload, which names its files in `changes[]` (or inside the patch text) and
// can cover several files at once. Everything that resolves "what file is this
// tool acting on" goes through here so both engines render identically.

/** `input` keys that hold a concrete file path. */
const FILE_PATH_KEYS = ['file_path', 'notebook_path', 'path']

// `*** Add File: x`, `*** Update File: x`, `*** Delete File: x` — the envelope
// Codex `apply_patch` calls carry instead of a structured path.
const PATCH_FILE_RE = /^\*\*\* (?:Add|Update|Delete) File: (.+)$/gm

export function asObj(v: unknown): Record<string, unknown> {
  return v && typeof v === 'object' ? (v as Record<string, unknown>) : {}
}

/**
 * Every file a single tool_use touches, in the order they appear.
 *
 * A Set, not an array: one payload can name the same file twice (e.g. a `path`
 * key alongside a `changes` entry) and that must not inflate the touch count.
 */
export function toolFileTargets(input: unknown): string[] {
  const obj = asObj(input)
  const out = new Set<string>()
  const push = (v: unknown) => {
    if (typeof v === 'string' && v.trim()) out.add(v.trim())
  }

  for (const k of FILE_PATH_KEYS) push(obj[k])

  // Codex `fileChange`: `changes` is either an array of `{ path, kind, diff }`
  // or an object keyed by path, depending on the app-server payload.
  const changes = obj.changes
  if (Array.isArray(changes)) {
    for (const c of changes) push(typeof c === 'string' ? c : asObj(c).path)
  } else if (changes && typeof changes === 'object') {
    for (const k of Object.keys(changes)) push(k)
  }

  // Codex `apply_patch`: the patch text itself names the files.
  for (const k of ['input', 'patch']) {
    const raw = obj[k]
    if (typeof raw !== 'string') continue
    for (const m of raw.matchAll(PATCH_FILE_RE)) push(m[1])
  }

  return [...out]
}

/** The primary file a tool acts on, or null when the tool isn't file-based. */
export function toolFileTarget(input: unknown): string | null {
  return toolFileTargets(input)[0] ?? null
}

/** One rendered diff block: a before/after pair, tagged with its file. */
export interface DiffPart {
  before: string
  after: string
  /** Set only for Codex payloads, where one call can span several files. */
  path?: string
}

function asStr(v: unknown): string {
  return typeof v === 'string' ? v : ''
}

/**
 * Split a unified diff body into one before/after pair per `@@` hunk. Hunks stay
 * apart on purpose: joining them would glue non-adjacent regions of the file
 * together and invent changes that aren't there.
 */
export function unifiedHunks(diff: string): DiffPart[] {
  const parts: { before: string[]; after: string[] }[] = []
  let cur: { before: string[]; after: string[] } | null = null
  for (const line of diff.split('\n')) {
    if (line.startsWith('@@')) {
      cur = { before: [], after: [] }
      parts.push(cur)
      continue
    }
    // File headers (`--- a/x`, `+++ b/x`, `diff --git …`) precede the first hunk.
    if (!cur && (/^(---|\+\+\+) /.test(line) || line.startsWith('diff --git') || line.startsWith('index '))) continue
    if (line.startsWith('\\')) continue // "\ No newline at end of file"
    if (!cur) {
      // Headerless patch body — treat the whole thing as a single hunk.
      cur = { before: [], after: [] }
      parts.push(cur)
    }
    if (line.startsWith('+')) cur.after.push(line.slice(1))
    else if (line.startsWith('-')) cur.before.push(line.slice(1))
    else {
      const text = line.startsWith(' ') ? line.slice(1) : line
      cur.before.push(text)
      cur.after.push(text)
    }
  }
  return parts
    .map((p) => ({ before: p.before.join('\n'), after: p.after.join('\n') }))
    .filter((p) => p.before || p.after)
}

/**
 * Codex `fileChange`: `changes` lists `{ path, kind, diff }`, where the diff is a
 * unified patch for an update and the whole file body for an add/delete. Claude
 * sends `old_string`/`new_string` instead, which callers handle themselves.
 */
export function codexEditParts(input: unknown): DiffPart[] {
  const changes = asObj(input).changes
  const list: unknown[] = Array.isArray(changes)
    ? changes
    : changes && typeof changes === 'object'
      ? Object.entries(changes).map(([path, c]) => ({ path, ...asObj(c) }))
      : []
  const out: DiffPart[] = []
  for (const raw of list) {
    const c = asObj(raw)
    const path = asStr(c.path)
    const body = asStr(c.diff) || asStr(c.content) || asStr(c.patch)
    if (!body) continue
    const kind = typeof c.kind === 'string' ? c.kind : asStr(asObj(c.kind).type)
    // An add/delete carries the plain file body — unless the payload sent a
    // patch anyway, which `unifiedHunks` still handles.
    if ((kind === 'add' || kind === 'delete') && !body.startsWith('@@')) {
      out.push(kind === 'add' ? { before: '', after: body, path } : { before: body, after: '', path })
      continue
    }
    for (const part of unifiedHunks(body)) out.push({ ...part, path })
  }
  return out
}
