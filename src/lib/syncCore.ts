// Pure helpers for two-way sync (Settings → Sync and the sidebar indicator).
// No Tauri / Vue imports, so node:test can load this file directly.

export type SyncState = 'disabled' | 'connecting' | 'online' | 'offline' | 'error'

/** `sync_status` / the `sync://status` event (Rust `SyncStatus`). */
export interface SyncStatus {
  state: SyncState
  paused: boolean
  /** Unix millis of the last successful push / pull. */
  last_push: number | null
  last_pull: number | null
  pending: number
  /** Rows whose latest change the relay permanently refused (not synced until retried or changed). */
  parked: number
  last_error: string | null
}

export const DISABLED_STATUS: SyncStatus = {
  state: 'disabled',
  paused: false,
  last_push: null,
  last_pull: null,
  pending: 0,
  parked: 0,
  last_error: null,
}

/** A row the relay refused for good (`sync_parked_list`, Rust `store::ParkedRow`). */
export interface ParkedRow {
  tbl: string
  /** Readable primary key (`id`, or `k=v, k=v`). */
  pk: string
  /** `too_large` | `bad_request`. */
  reason: string
  /** Unix millis. */
  parked_at: number
}

/** One device of the group (Rust `sync::leader::DeviceInfo`). */
export interface SyncDevice {
  id: string
  name: string
  last_seen: number | null
  is_self: boolean
  is_leader: boolean
  /** Created the group: the only device the relay lets remove others. */
  is_creator: boolean
}

/** A device asking to join this one's group (`sync://join-request`). */
export interface JoinRequest {
  request_id: string
  device_name: string
  /** Unix millis after which it is denied unanswered. */
  expires_at: number
}

/** A project on this machine and where it lives (Rust `sync::bootstrap::ProjectMapping`). */
export interface ProjectMapping {
  project_id: string
  name: string
  /** Local checkout root. */
  path: string
  /** The root as synced to the other devices. */
  synced_path: string
  /** Set by the first-join match or by hand. */
  mapped: boolean
  /** `false` = the checkout does not exist on this machine. */
  on_this_machine: boolean
}

/** Per-table merge counts (Rust `TableCounts`). */
export interface TableCounts {
  tbl: string
  matched: number
  rekeyed: number
  new_remote: number
  new_local: number
  conflicts: number
  deleted_remote: number
}

/** How a synced project matched a local one (Rust `ProjectMatch`). */
export interface ProjectMatch {
  remote_id: string
  name: string
  synced_path: string
  local_id: string | null
  /** `id` | `path` | `git_remote` | `name`; null = new on this machine. */
  matched_by: string | null
  local_path: string
  on_this_machine: boolean
}

/** `sync_bootstrap_preview` (Rust `BootstrapPreview`): totals + per table + projects. */
export interface BootstrapPreview {
  matched: number
  rekeyed: number
  new_remote: number
  new_local: number
  conflicts: number
  deleted_remote: number
  tables: TableCounts[]
  projects: ProjectMatch[]
  undecryptable: number
}

/** `sync_bootstrap_commit` (Rust `BootstrapReport`). */
export interface BootstrapReport {
  preview: BootstrapPreview
  applied: { applied: number; skipped_older: number; conflicts: number; deferred: number }
  /** Local rows queued for push. */
  queued: number
}

/** The count columns of a preview, in display order. */
export const COUNT_KEYS = ['new_remote', 'matched', 'rekeyed', 'new_local', 'conflicts', 'deleted_remote'] as const
export type CountKey = (typeof COUNT_KEYS)[number]

/** Non-zero totals of a preview as `[key, n]` rows. */
export function totalRows(p: Pick<BootstrapPreview, CountKey>): Array<[CountKey, number]> {
  return COUNT_KEYS.filter((k) => p[k] > 0).map((k) => [k, p[k]])
}

/** Tables with something to report, with a compact "+3 new · 2 matched" summary. */
export function tableRows(p: Pick<BootstrapPreview, 'tables'>): Array<{ tbl: string; counts: Array<[CountKey, number]> }> {
  return p.tables
    .map((t) => ({ tbl: t.tbl, counts: totalRows(t) }))
    .filter((r) => r.counts.length > 0)
}

/** Synced projects the merge could not place on this machine. */
export function missingProjects(p: Pick<BootstrapPreview, 'projects'>): ProjectMatch[] {
  return p.projects.filter((m) => !m.on_this_machine)
}

/**
 * Status dot state, most urgent first: merge pending, error, paused, offline,
 * parked rows, syncing (connecting / backlog), ok.
 */
export type SyncTone = 'off' | 'ok' | 'syncing' | 'merge' | 'parked' | 'paused' | 'offline' | 'error'

export function syncTone(s: SyncStatus | null | undefined, mergePending = false): SyncTone {
  if (mergePending) return 'merge'
  if (!s || s.state === 'disabled') return 'off'
  if (s.state === 'error') return 'error'
  if (s.paused) return 'paused'
  if (s.state === 'offline') return 'offline'
  if (s.parked > 0) return 'parked'
  if (s.state === 'connecting' || s.pending > 0) return 'syncing'
  return 'ok'
}

/** Tailwind classes of the dot per tone. */
export const SYNC_DOT_CLASS: Record<SyncTone, string> = {
  off: 'bg-muted-foreground/40',
  ok: 'bg-emerald-500',
  syncing: 'bg-sky-500',
  merge: 'bg-violet-500',
  parked: 'bg-orange-500',
  paused: 'bg-muted-foreground',
  offline: 'bg-amber-500',
  error: 'bg-destructive',
}

/** i18n key (under `settings.sync.state`) for the status label. */
export function syncStateKey(s: SyncStatus | null | undefined, mergePending = false): string {
  if (mergePending) return 'mergePending'
  if (!s || s.state === 'disabled') return 'disabled'
  if (s.paused) return 'paused'
  if (s.state === 'online' && s.parked > 0) return 'parked'
  return s.state
}

/**
 * Relative age of a unix-millis timestamp as `{ unit, n }` (`now` for under a
 * minute, `never` when absent), for an i18n "… ago" message.
 */
export function ageParts(ms: number | null | undefined, now: number = Date.now()):
  { unit: 'never' | 'now' | 'minutes' | 'hours' | 'days'; n: number } {
  if (ms == null || !Number.isFinite(ms)) return { unit: 'never', n: 0 }
  const s = Math.max(0, Math.floor((now - ms) / 1000))
  if (s < 60) return { unit: 'now', n: 0 }
  if (s < 3600) return { unit: 'minutes', n: Math.floor(s / 60) }
  if (s < 86_400) return { unit: 'hours', n: Math.floor(s / 3600) }
  return { unit: 'days', n: Math.floor(s / 86_400) }
}

/** Devices in display order: this one first, then the leader, then by name. */
export function sortDevices(list: SyncDevice[]): SyncDevice[] {
  return [...list].sort(
    (a, b) =>
      Number(b.is_self) - Number(a.is_self) ||
      Number(b.is_leader) - Number(a.is_leader) ||
      (a.name || a.id).localeCompare(b.name || b.id),
  )
}

/** Short device label: its name, else the start of its id. */
export function deviceLabel(d: Pick<SyncDevice, 'id' | 'name'>): string {
  return d.name.trim() || d.id.slice(0, 8)
}

/** The group creator, when known (groups made by older builds have none). */
export function groupCreator(list: SyncDevice[]): SyncDevice | null {
  return list.find((d) => d.is_creator) ?? null
}

/**
 * Whether this device may remove others: it created the group, or no creator
 * is recorded (older group; the relay still has the last word).
 */
export function canRemoveDevices(list: SyncDevice[]): boolean {
  const creator = groupCreator(list)
  return !creator || creator.is_self
}

/** Time left until `expiresAt` (unix millis) as `m:ss`, never negative. */
export function countdown(expiresAt: number | null | undefined, now: number = Date.now()): string {
  if (expiresAt == null || !Number.isFinite(expiresAt)) return '0:00'
  const s = Math.max(0, Math.ceil((expiresAt - now) / 1000))
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`
}

/**
 * Backend failure codes with a translated message (`settings.sync.errors.<code>`): join / create
 * failures, the merge refusal, and the relay's sync error codes.
 */
export const SYNC_ERROR_CODES = [
  'host_offline',
  'wrong_password',
  'host_not_in_group',
  'host_unsupported',
  'not_accepting',
  'declined',
  'busy',
  'relay_not_configured',
  'relay_error',
  'sync_bootstrap_runs_live',
  'sync_bootstrap_not_pending',
  'sync_bootstrap_in_progress',
  'sync_leave_handoff_pending',
  'auth_failed',
  'relay_behind',
  'ticket_required',
  'ticket_invalid',
  'group_exists',
  'forbidden',
  'quota_exceeded',
  'device_revoked',
] as const
export type SyncErrorCode = (typeof SYNC_ERROR_CODES)[number]

/** Codes the relay sends; the backend wraps them ("relay rejected this device: device_revoked"). */
const RELAY_CODES: readonly SyncErrorCode[] = [
  'auth_failed',
  'relay_behind',
  'ticket_required',
  'ticket_invalid',
  'group_exists',
  'forbidden',
  'quota_exceeded',
  'device_revoked',
]

/** Backend messages that carry no code. */
const PHRASES: Array<[string, SyncErrorCode]> = [
  ['only the device that created the group can remove', 'forbidden'],
  ['relay not configured', 'relay_not_configured'],
]

/**
 * The known failure code of a backend error (a `code:` prefix, a relay code anywhere in it, or a
 * known phrase), else null (show it raw).
 */
export function syncErrorCode(message: unknown): SyncErrorCode | null {
  const m = String(message ?? '').trim()
  const prefixed = SYNC_ERROR_CODES.find((c) => m === c || m.startsWith(`${c}:`))
  if (prefixed) return prefixed
  const relay = RELAY_CODES.find((c) => new RegExp(`(^|[^a-z_])${c}($|[^a-z_])`).test(m))
  if (relay) return relay
  const lower = m.toLowerCase()
  return PHRASES.find(([p]) => lower.includes(p))?.[1] ?? null
}
