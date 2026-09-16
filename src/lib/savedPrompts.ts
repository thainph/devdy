/**
 * The user's library of frequently-used prompts.
 *
 * Kept deliberately small: the list lives in the `saved_prompts` app setting as
 * a JSON string (no table, no migration), edited in Settings → Prompt Templates
 * and picked from a dropdown in the run composer.
 */
export interface SavedPrompt {
  id: string
  title: string
  body: string
}

export function newPromptId(): string {
  return crypto.randomUUID()
}

/**
 * Parse the `saved_prompts` setting. Never throws: a malformed or hand-edited
 * value degrades to an empty library rather than breaking Settings and the
 * composer.
 */
export function parseSavedPrompts(raw: string | null | undefined): SavedPrompt[] {
  if (!raw) return []
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch {
    return []
  }
  if (!Array.isArray(parsed)) return []
  return parsed.flatMap((item) => {
    if (!item || typeof item !== 'object') return []
    const { id, title, body } = item as Record<string, unknown>
    if (typeof body !== 'string') return []
    return [
      {
        id: typeof id === 'string' && id ? id : newPromptId(),
        title: typeof title === 'string' ? title : '',
        body,
      },
    ]
  })
}

/** Drop rows the user left completely blank, then serialize for storage. */
export function serializeSavedPrompts(prompts: SavedPrompt[]): string {
  const kept = prompts.filter((p) => p.title.trim() || p.body.trim())
  return JSON.stringify(kept)
}

/** Label shown in the composer dropdown for a prompt with no title yet. */
export function promptLabel(p: SavedPrompt): string {
  const title = p.title.trim()
  if (title) return title
  const firstLine = p.body.trim().split('\n')[0] ?? ''
  return firstLine.length > 60 ? `${firstLine.slice(0, 60)}…` : firstLine
}
