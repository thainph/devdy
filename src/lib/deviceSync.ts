// Device pull sync: wire types + pure helpers for the Sync dialog. Mirrors the
// `device_sync_*` commands (one-shot pull from another online Devdy, overwrites
// local data per category — see docs/device-pull-sync-plan.md).

export interface SyncDevice {
  deviceId: string
  alias: string
  appVersion: string
}

export const SYNC_CATEGORY_KEYS = [
  'todos', 'notes', 'secretNotes', 'slackThreads', 'skillsRules',
  'projects', 'settings', 'mcpServers', 'vpsServers', 'accounts',
] as const

export type SyncCategory = (typeof SYNC_CATEGORY_KEYS)[number]
export type SyncCategories = Record<SyncCategory, boolean>

export interface SyncCounts {
  created: number
  updated: number
  unchanged: number
  deleted: number
  skipped: number
}

export interface SyncReport {
  counts: Record<string, SyncCounts>
  warnings: string[]
}

export type SyncPhase = 'connecting' | 'auth' | 'downloading' | 'applying' | 'done'

export interface SyncProgress {
  phase: SyncPhase
  received: number
  total: number
}

/** Tauri event carrying `SyncProgress` while a pull runs. */
export const DEVICE_SYNC_PROGRESS = 'device-sync-progress'

export const SYNC_PHASES: SyncPhase[] = ['connecting', 'auth', 'downloading', 'applying', 'done']

/** Every category ticked: the dialog's starting state. */
export function defaultCategories(): SyncCategories {
  return Object.fromEntries(SYNC_CATEGORY_KEYS.map(k => [k, true])) as SyncCategories
}

export function selectedCategories(c: SyncCategories): SyncCategory[] {
  return SYNC_CATEGORY_KEYS.filter(k => c[k])
}

export type SyncErrorKind = 'wrongPassword' | 'deviceOffline' | 'other'

/** Map a command error (plain string) to a known kind; `other` is shown verbatim. */
export function syncErrorKind(err: unknown): SyncErrorKind {
  const s = String(err ?? '').trim()
  if (s === 'wrong_password') return 'wrongPassword'
  if (s === 'device_offline') return 'deviceOffline'
  return 'other'
}

/** 0–100 for the progress bar; download progress only counts once the size is known. */
export function progressPercent(p: SyncProgress | null): number {
  if (!p) return 0
  if (p.phase === 'done') return 100
  if (p.phase === 'downloading' && p.total > 0) {
    return Math.min(100, Math.max(0, Math.round((p.received / p.total) * 100)))
  }
  return p.phase === 'applying' ? 100 : 0
}

export interface SyncReportRow {
  key: string
  counts: SyncCounts
}

const EMPTY_COUNTS: SyncCounts = { created: 0, updated: 0, unchanged: 0, deleted: 0, skipped: 0 }

/**
 * Report rows in a stable order: the category order first, then any extra keys the
 * backend reports (finer sections such as `skills` / `githubAccounts`) alphabetically.
 * Missing numbers default to 0.
 */
export function reportRows(report: SyncReport): SyncReportRow[] {
  const known = new Set<string>(SYNC_CATEGORY_KEYS)
  const keys = Object.keys(report.counts ?? {})
  const ordered = [
    ...SYNC_CATEGORY_KEYS.filter(k => keys.includes(k)),
    ...keys.filter(k => !known.has(k)).sort(),
  ]
  return ordered.map(key => ({ key, counts: { ...EMPTY_COUNTS, ...report.counts[key] } }))
}

export function reportTotals(report: SyncReport): SyncCounts {
  const total = { ...EMPTY_COUNTS }
  for (const row of reportRows(report)) {
    for (const k of Object.keys(total) as (keyof SyncCounts)[]) total[k] += row.counts[k] || 0
  }
  return total
}
