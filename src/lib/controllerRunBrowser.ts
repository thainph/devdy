/**
 * The phone run browser's project level. The Host sends projects and runs in
 * the desktop's own order (projects: manual order, then usage; runs: pinned,
 * then last activity), so nothing here re-sorts — the phone must list them
 * exactly like the desktop does. Pure helper; the view is
 * `src/controller/components/RunBrowserView.vue`.
 */

export interface BrowserProject {
  id: string
  name: string
}

export interface BrowserRun {
  id: string
  project_id: string
  project_name?: string | null
}

export interface BrowserProjectRow {
  id: string
  name: string
  /** Runs listed for the project. */
  total: number
  /** Of those, how many await a permission decision. */
  pending: number
}

/**
 * One row per project in the Host's project order — including projects with
 * no session yet, as the desktop sidebar shows them. A run whose project the
 * list lacks (an older Host that refuses `list_projects`) still gets a row,
 * after the known ones, in the order its first run appears.
 */
export function browserProjectRows(
  projects: readonly BrowserProject[],
  runs: readonly BrowserRun[],
  pendingRunIds: ReadonlySet<string>,
  fallbackName: string,
): BrowserProjectRow[] {
  const byId = new Map<string, BrowserProjectRow>()
  for (const p of projects) {
    byId.set(p.id, { id: p.id, name: p.name.trim() || fallbackName, total: 0, pending: 0 })
  }
  for (const run of runs) {
    let row = byId.get(run.project_id)
    if (!row) {
      row = { id: run.project_id, name: run.project_name?.trim() || fallbackName, total: 0, pending: 0 }
      byId.set(run.project_id, row)
    }
    row.total += 1
    if (pendingRunIds.has(run.id)) row.pending += 1
  }
  return [...byId.values()]
}
