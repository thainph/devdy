// Shared helpers for turning text references (inline `<code>` filenames, relative
// markdown links) into clickable links to files in a project. Used by RunView's
// own markdown containers and by the reusable FileViewer component.

/**
 * Resolve a raw token (an inline-code filename or a relative link href) to a
 * concrete project-relative path, or null when it doesn't unambiguously match a
 * known project file. `fileSet` is the set of project-relative file paths.
 */
export function matchProjectFile(raw: string, fileSet: Set<string>): string | null {
  if (!raw || raw.length > 200 || /\s/.test(raw)) return null
  let token = raw.trim().replace(/[)\].,:;'"`]+$/, '')
  if (token.startsWith('./')) token = token.slice(2)
  if (!token) return null
  if (fileSet.has(token)) return token
  // Basename / suffix match: unambiguous tail like `dir/file.ts`.
  if (token.includes('/')) {
    for (const p of fileSet) if (p === token || p.endsWith('/' + token)) return p
  } else if (/\.[a-zA-Z0-9]+$/.test(token)) {
    // Bare filename with an extension — match only if exactly one file ends with it.
    let hit: string | null = null
    for (const p of fileSet) {
      if (p === token || p.endsWith('/' + token)) {
        if (hit) return null // ambiguous
        hit = p
      }
    }
    if (hit) return hit
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
