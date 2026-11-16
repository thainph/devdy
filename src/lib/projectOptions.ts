// One order for every project picker / filter: by name, case-insensitive and
// numeric-aware ("Project 2" before "Project 10"), id as the tie-break so the
// order is stable. The sidebar / Projects screen keep the drag-and-drop order;
// pickers use this so a project is always where you expect it.

export interface ProjectOption {
  value: string
  label: string
}

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' })

export function compareProjectNames(a: string, b: string): number {
  return collator.compare(a, b)
}

/** Projects sorted by name (a copy; the input is left untouched). */
export function sortProjectsByName<T extends { id: string; name: string }>(projects: readonly T[]): T[] {
  return [...projects].sort((a, b) => compareProjectNames(a.name, b.name) || a.id.localeCompare(b.id))
}

/** `{ value: id, label: name }` options, sorted by name. */
export function projectOptions(projects: readonly { id: string; name: string }[]): ProjectOption[] {
  return sortProjectsByName(projects).map((p) => ({ value: p.id, label: p.name }))
}

/** Options for just these project ids (e.g. the ones that have items), named
 * via `nameOf` — an unknown id is labelled with the id itself — sorted by name. */
export function projectOptionsForIds(
  ids: Iterable<string>,
  nameOf: (id: string) => string | undefined,
): ProjectOption[] {
  return projectOptions([...new Set(ids)].map((id) => ({ id, name: nameOf(id) ?? id })))
}
