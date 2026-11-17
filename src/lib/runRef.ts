/**
 * Identity line for an issue/PR run — "acme/web #42 · @octo" — so reviews across
 * many repos are told apart at a glance in the History list and run header.
 */

export interface RunRefSource {
  run_type: string
  ref_number: number | null
  ref_author?: string | null
}

/** Short repo name for one-line rows: the last path segment of a slug
 *  ("group/sub/web" → "web"). */
export function repoShortName(slug: string): string {
  return slug.split('/').filter(Boolean).pop() ?? slug
}

/**
 * "<repo> #<n> · @<author>" for an issue/PR run, or null for a session. Missing
 * pieces are left out. `label` is what the row already shows as its title: the
 * author is skipped when it already reads "@author" (PR inbox reviews are titled
 * "PR #N · @author · title").
 */
export function runRefLine(run: RunRefSource, repoSlug: string | null, label = ''): string | null {
  if (run.run_type === 'session') return null
  const ref = [repoSlug, run.ref_number != null ? `#${run.ref_number}` : null].filter(Boolean).join(' ')
  const author = run.ref_author?.trim()
  const parts = [ref]
  if (author && !mentions(label, author)) parts.push(`@${author}`)
  const line = parts.filter(Boolean).join(' · ')
  return line || null
}

/** Whether `label` contains "@login" as a whole handle ("@octo" ≠ "@octopus"). */
function mentions(label: string, login: string): boolean {
  let i = label.indexOf(`@${login}`)
  while (i !== -1) {
    const next = label[i + login.length + 1]
    if (next === undefined || !/[\w-]/.test(next)) return true
    i = label.indexOf(`@${login}`, i + 1)
  }
  return false
}
