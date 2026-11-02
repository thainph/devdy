/**
 * The phone Controller's shared project scope: one project (or every project)
 * that Sessions, Todo, Notes and the Setting screens all follow, picked from
 * a searchable sheet that lists recently used projects first. Pure helpers;
 * the state lives in `src/controller/App.vue`.
 */

export interface ScopeProject {
  id: string
  name: string
  path?: string
}

/** How many recently used projects are remembered. */
export const RECENT_LIMIT = 5

/** `id` moved to the front of `recent`, deduplicated and capped. */
export function rememberProject(recent: readonly string[], id: string, limit = RECENT_LIMIT): string[] {
  return [id, ...recent.filter((r) => r !== id)].slice(0, limit)
}

/** Lower-case and strip diacritics, so "dự án" matches "du an". */
export function foldText(s: string): string {
  return s
    .normalize('NFD')
    .replace(/[̀-ͯ]/g, '')
    .replace(/đ/g, 'd')
    .replace(/Đ/g, 'D')
    .toLowerCase()
}

/**
 * The picker's rows: projects matching `query` (name or path, accent- and
 * case-insensitive), split into the recently used ones (in recency order) and
 * the rest (in host order). While searching, everything is one flat list.
 */
export function pickerRows<T extends ScopeProject>(
  projects: readonly T[],
  recent: readonly string[],
  query: string,
): { recent: T[]; others: T[] } {
  const q = foldText(query.trim())
  if (q) {
    const hit = (p: T) => foldText(p.name).includes(q) || (p.path ? foldText(p.path).includes(q) : false)
    return { recent: [], others: projects.filter(hit) }
  }
  const byId = new Map(projects.map((p) => [p.id, p]))
  const first = recent.map((id) => byId.get(id)).filter((p): p is T => p !== undefined)
  const seen = new Set(first.map((p) => p.id))
  return { recent: first, others: projects.filter((p) => !seen.has(p.id)) }
}

/**
 * The project a screen that needs one (the Setting switches) works on: the
 * scope when it names a known project, else the most recently used known
 * project, else the first one; null when the host has none.
 */
export function concreteProject(
  projects: readonly ScopeProject[],
  scope: string | null,
  recent: readonly string[],
): string | null {
  const known = new Set(projects.map((p) => p.id))
  if (scope && known.has(scope)) return scope
  return recent.find((id) => known.has(id)) ?? projects[0]?.id ?? null
}

/** A stored scope that no longer names a known project means "all". Before
 * the project list arrives (empty), the stored value is kept. */
export function validScope(projects: readonly ScopeProject[], scope: string | null): string | null {
  if (!scope || !projects.length) return scope
  return projects.some((p) => p.id === scope) ? scope : null
}
