// Shared helpers for turning text references (inline `<code>` filenames, relative
// markdown links) into clickable links to files in a project. Used by RunView's
// own markdown containers and by the reusable FileViewer component.

// A project can hold ~50k files and every inline `<code>` token of every
// rendered message is resolved against them, so a linear `endsWith` scan per
// token adds up fast. Index the set once (basename → paths, in set order) and
// memoize per token; a suffix lookup then only walks the few files sharing the
// token's basename.
interface FileIndex {
  byBase: Map<string, string[]>
  memo: Map<string, string | null>
}

/** Tokens memoized per index before the memo is dropped and rebuilt. */
const MEMO_MAX = 5000

// Keyed by the Set itself: callers build a fresh Set whenever the file list
// changes (and must not mutate one after use), so identity is the version.
const indexCache = new WeakMap<Set<string>, FileIndex>()

function basename(p: string): string {
  return p.slice(p.lastIndexOf('/') + 1)
}

function indexFor(fileSet: Set<string>): FileIndex {
  let idx = indexCache.get(fileSet)
  if (idx) return idx
  const byBase = new Map<string, string[]>()
  for (const p of fileSet) {
    const b = basename(p)
    const list = byBase.get(b)
    if (list) list.push(p)
    else byBase.set(b, [p])
  }
  idx = { byBase, memo: new Map() }
  indexCache.set(fileSet, idx)
  return idx
}

/**
 * Resolve a raw token (an inline-code filename or a relative link href) to a
 * concrete project-relative path, or null when it doesn't unambiguously match a
 * known project file. `fileSet` is the set of project-relative file paths.
 */
export function matchProjectFile(raw: string, fileSet: Set<string>): string | null {
  if (!raw || raw.length > 200 || /\s/.test(raw)) return null
  const idx = indexFor(fileSet)
  const hit = idx.memo.get(raw)
  if (hit !== undefined) return hit
  const path = resolveToken(raw, fileSet, idx)
  if (idx.memo.size >= MEMO_MAX) idx.memo.clear()
  idx.memo.set(raw, path)
  return path
}

function resolveToken(raw: string, fileSet: Set<string>, idx: FileIndex): string | null {
  let token = raw.trim().replace(/[)\].,:;'"`]+$/, '')
  if (token.startsWith('./')) token = token.slice(2)
  if (!token) return null
  if (fileSet.has(token)) return token
  // Basename / suffix match: unambiguous tail like `dir/file.ts`. A path ending
  // in `/token` necessarily shares the token's basename, so only those are
  // candidates — first in set order wins, as with a full scan.
  const suffix = '/' + token
  if (token.includes('/')) {
    const base = basename(token)
    if (!base) return null // `dir/` — no file path ends in a slash
    for (const p of idx.byBase.get(base) ?? []) if (p.endsWith(suffix)) return p
  } else if (/\.[a-zA-Z0-9]+$/.test(token)) {
    // Bare filename with an extension — match only if exactly one file ends with
    // it. (A path equal to the token was already returned above, so every
    // same-basename path ends with `/token`.)
    const list = idx.byBase.get(token)
    if (list?.length === 1) return list[0]
  }
  return null
}

/** Extract a target line number from a `#L42` / `#42` / `:42` / `:42-50` reference. */
export function parseLineRef(ref: string): number | null {
  const m = ref.match(/#L?(\d+)/i) ?? ref.match(/:(\d+)(?:-\d+)?$/)
  return m ? parseInt(m[1], 10) : null
}

/**
 * Mark inline `<code>` elements that name a project file as clickable
 * `code.file-link[data-file-path]`. `matcher` resolves a token to a path.
 */
export function decorateFileLinks(el: HTMLElement, matcher: (raw: string) => string | null) {
  el.querySelectorAll('code').forEach((code) => {
    if (code.closest('pre')) return
    const path = matcher((code.textContent ?? '').trim())
    if (path) {
      code.classList.add('file-link')
      code.setAttribute('data-file-path', path)
    } else {
      code.classList.remove('file-link')
      code.removeAttribute('data-file-path')
    }
  })
}
