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
/** Ticked categories + `mirror`: also delete local items the source doesn't have. */
export type SyncCategories = Record<SyncCategory, boolean> & { mirror: boolean }

/**
 * Categories whose local-only items `mirror` deletes. Secret notes, Slack threads, projects
 * and general settings are never deleted.
 */
export const MIRROR_DELETE_KEYS = [
  'todos', 'notes', 'skillsRules', 'mcpServers', 'vpsServers', 'accounts',
] as const satisfies readonly SyncCategory[]

export interface SyncCounts {
  created: number
  updated: number
  unchanged: number
  deleted: number
  skipped: number
}

export interface SyncReport {
  counts: Record<string, SyncCounts>
  warnings: SyncWarning[]
}

/**
 * One report line as `code` + `params` (i18n `deviceSync.warnings.<code>`). Codes:
 * `project_folder_missing` {project, path}, `link_target_missing` {project, kind, name},
 * `link_failed` {project, name, error}, `project_failed` {project, error},
 * `item_failed` {section, name, error}, `project_sync_failed` {section, name, detail},
 * `category_not_sent` / `category_empty_no_mirror` {section},
 * `delete_failed` {section, name, error}, `other` {message}.
 */
export interface SyncWarning {
  code: string
  params: Record<string, string>
}

export type SyncWarningGroup = 'folders' | 'links' | 'mirror' | 'problems'

const WARNING_GROUP: Record<string, SyncWarningGroup> = {
  project_folder_missing: 'folders',
  link_target_missing: 'links',
  link_failed: 'links',
  category_not_sent: 'mirror',
  category_empty_no_mirror: 'mirror',
}

const WARNING_GROUP_ORDER: SyncWarningGroup[] = ['problems', 'folders', 'links', 'mirror']

/** Warnings shown per group before "show N more". */
export const WARNING_PREVIEW = 3

/** A plain string (older report shape) reads as an `other` warning. */
export function normalizeWarning(w: SyncWarning | string): SyncWarning {
  if (typeof w === 'string') return { code: 'other', params: { message: w } }
  return { code: w.code || 'other', params: w.params ?? {} }
}

/**
 * Warnings by group, real problems first, then folder notes, missing link targets and
 * mirror notes; empty groups dropped, order inside a group kept. Exact repeats are dropped.
 */
export function groupWarnings(warnings: (SyncWarning | string)[]): { group: SyncWarningGroup, items: SyncWarning[] }[] {
  const seen = new Set<string>()
  const byGroup = new Map<SyncWarningGroup, SyncWarning[]>()
  for (const raw of warnings ?? []) {
    const w = normalizeWarning(raw)
    const key = JSON.stringify([w.code, Object.entries(w.params).sort()])
    if (seen.has(key)) continue
    seen.add(key)
    const group = WARNING_GROUP[w.code] ?? 'problems'
    byGroup.set(group, [...(byGroup.get(group) ?? []), w])
  }
  return WARNING_GROUP_ORDER.filter(g => byGroup.has(g)).map(group => ({ group, items: byGroup.get(group)! }))
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

/** Every category ticked, mirror off: the dialog's starting state. */
export function defaultCategories(): SyncCategories {
  return { ...Object.fromEntries(SYNC_CATEGORY_KEYS.map(k => [k, true])), mirror: false } as SyncCategories
}

export function selectedCategories(c: SyncCategories): SyncCategory[] {
  return SYNC_CATEGORY_KEYS.filter(k => c[k])
}

/** Ticked categories where local-only items get deleted (empty unless `mirror` is on). */
export function mirrorDeletions(c: SyncCategories): SyncCategory[] {
  if (!c.mirror) return []
  return MIRROR_DELETE_KEYS.filter(k => c[k])
}

/** Error codes the `device_sync_*` commands return as plain strings (i18n `deviceSync.errors.<code>`). */
export const SYNC_ERROR_CODES = [
  'wrong_password', 'device_offline', 'rate_limited', 'checksum_mismatch', 'timeout',
  'device_disconnected', 'source_failed', 'bad_request', 'bundle_too_large', 'bundle_corrupt',
  'bundle_unsupported_version', 'protocol_error',
  'relay_not_configured', 'relay_unreachable', 'relay_auth_failed', 'relay_unsupported', 'busy',
] as const

export type SyncErrorCode = (typeof SYNC_ERROR_CODES)[number]

/** The known code of a command error, or `null` (then the error is shown verbatim). */
export function syncErrorCode(err: unknown): SyncErrorCode | null {
  const s = String(err ?? '').trim()
  return (SYNC_ERROR_CODES as readonly string[]).includes(s) ? (s as SyncErrorCode) : null
}

/** Codes after which the picked device may be gone: the device list is refreshed. */
export function isDeviceGone(code: SyncErrorCode | null): boolean {
  return code === 'device_offline' || code === 'device_disconnected'
}

/** Display name of a device; the alias may be empty. */
export function deviceLabel(d: Pick<SyncDevice, 'alias' | 'deviceId'>): string {
  return d.alias.trim() || `Devdy · ${d.deviceId.slice(0, 8)}`
}

/**
 * Display names for a device list, by device id. Devices sharing a name get their short
 * id appended (`macbook · a1b2c3d4`) so the user can tell them apart.
 */
export function deviceLabels(devices: Pick<SyncDevice, 'alias' | 'deviceId'>[]): Record<string, string> {
  const count = new Map<string, number>()
  for (const d of devices) {
    const l = deviceLabel(d)
    count.set(l, (count.get(l) ?? 0) + 1)
  }
  return Object.fromEntries(devices.map((d) => {
    const l = deviceLabel(d)
    return [d.deviceId, (count.get(l) ?? 0) > 1 && d.alias.trim() ? `${l} · ${d.deviceId.slice(0, 8)}` : l]
  }))
}

/** Categories as the dialog groups them (i18n `deviceSync.groups.<key>`). */
export const SYNC_GROUPS: { key: 'work' | 'library' | 'config', items: SyncCategory[] }[] = [
  { key: 'work', items: ['todos', 'notes', 'secretNotes', 'slackThreads'] },
  { key: 'library', items: ['skillsRules', 'projects'] },
  { key: 'config', items: ['settings', 'mcpServers', 'vpsServers', 'accounts'] },
]

/** Group checkbox state: every item ticked, none, or `mixed`. */
export function groupState(c: SyncCategories, items: readonly SyncCategory[]): boolean | 'mixed' {
  const on = items.filter(k => c[k]).length
  return on === items.length ? true : on === 0 ? false : 'mixed'
}

/** The last successful pull on this device (`device_sync_last`). */
export interface LastSync {
  at: string
  sourceDeviceId: string
  sourceAlias: string
  categories: number
  created: number
  updated: number
  deleted: number
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

/** How far back `at` is from `now`, for "today 14:32" / "yesterday 14:32" / a date. */
export function syncDay(at: string, now: Date = new Date()): 'today' | 'yesterday' | 'earlier' | null {
  const d = new Date(at)
  if (Number.isNaN(d.getTime())) return null
  const start = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime()
  if (d.getTime() >= start) return 'today'
  if (d.getTime() >= start - 86_400_000) return 'yesterday'
  return 'earlier'
}

/** The report columns shown in the result table: Same is never a column. */
export const REPORT_COLUMNS = ['created', 'updated', 'deleted', 'skipped'] as const
export type ReportColumn = (typeof REPORT_COLUMNS)[number]

/**
 * The compact result table: rows where something happened, the columns that are not
 * all zero there, and how many rows were left out because nothing changed in them.
 */
export function compactReport(report: SyncReport): { rows: SyncReportRow[], columns: ReportColumn[], hidden: number } {
  const all = reportRows(report)
  const rows = all.filter(r => REPORT_COLUMNS.some(c => r.counts[c] > 0))
  const columns = REPORT_COLUMNS.filter(c => rows.some(r => r.counts[c] > 0))
  return { rows, columns, hidden: all.length - rows.length }
}

/** Account kinds whose sign-in does not travel: they need a new login once synced. */
const RELOGIN_SECTIONS = ['claudeAccounts', 'awsAccounts', 'azureAccounts']

/** Whether the pull added or updated an account that needs signing in again here. */
export function needsRelogin(report: SyncReport): boolean {
  return RELOGIN_SECTIONS.some((k) => {
    const c = report.counts?.[k]
    return !!c && (c.created > 0 || c.updated > 0)
  })
}

export function reportTotals(report: SyncReport): SyncCounts {
  const total = { ...EMPTY_COUNTS }
  for (const row of reportRows(report)) {
    for (const k of Object.keys(total) as (keyof SyncCounts)[]) total[k] += row.counts[k] || 0
  }
  return total
}
