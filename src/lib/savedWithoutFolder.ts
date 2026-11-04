// Skill / rule links saved for a project whose folder isn't on this machine: the backend
// keeps the link and installs it once the folder appears (`commands/rules.rs`
// `SavedWithoutFolder`, `ApplyOneOutcome`). Pure helpers for the info toasts.

export interface SavedWithoutFolder {
  project_id: string
  project_name: string
}

/** `apply_skill` / `apply_rule`: the link was saved but nothing written yet. */
export interface ApplyOneOutcome {
  folder_missing: boolean
}

/** Distinct project names, in order, for "Saved for A, B — …". */
export function savedWithoutFolderNames(list: SavedWithoutFolder[] | null | undefined): string[] {
  const seen = new Set<string>()
  const names: string[] = []
  for (const p of list ?? []) {
    if (seen.has(p.project_id)) continue
    seen.add(p.project_id)
    names.push(p.project_name || p.project_id)
  }
  return names
}
