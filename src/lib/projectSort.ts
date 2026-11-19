// The one order every project picker / filter dropdown uses: A–Z by name,
// case- and accent-insensitive, with numbers compared by value ("app 2" before
// "app 10"). The Projects sidebar keeps its own drag-and-drop order.

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' })

/** Compare two project names for picker order (ties broken case-sensitively so the order is stable). */
export function compareProjectNames(a: string, b: string): number {
  return collator.compare(a.trim(), b.trim()) || (a < b ? -1 : a > b ? 1 : 0)
}

/** A sorted copy of `items`, A–Z by `nameOf` (defaults to `.name`). */
export function sortByProjectName<T extends { name: string }>(items: readonly T[]): T[]
export function sortByProjectName<T>(items: readonly T[], nameOf: (item: T) => string): T[]
export function sortByProjectName<T>(items: readonly T[], nameOf?: (item: T) => string): T[] {
  const name = nameOf ?? ((item: T) => (item as { name: string }).name)
  return [...items].sort((a, b) => compareProjectNames(name(a), name(b)))
}
