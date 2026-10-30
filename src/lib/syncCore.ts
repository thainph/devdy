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
  last_error: string | null
}

export const DISABLED_STATUS: SyncStatus = {
  state: 'disabled',
  paused: false,
  last_push: null,
  last_pull: null,
  pending: 0,
  last_error: null,
}

/** One device of the group (Rust `sync::leader::DeviceInfo`). */
export interface SyncDevice {
  id: string
  name: string
  last_seen: number | null
  is_self: boolean
  is_leader: boolean
}

/** A project known to the group, and where it lives on this machine. */
export interface ProjectMapping {
  remote_project_id: string
  name: string
  remote_path?: string | null
  local_path: string | null
}

/** Colour of the status dot. */
export type SyncTone = 'off' | 'ok' | 'busy' | 'warn' | 'error'

export function syncTone(s: SyncStatus | null | undefined): SyncTone {
  if (!s || s.state === 'disabled') return 'off'
  if (s.state === 'error') return 'error'
  if (s.paused || s.state === 'offline') return 'warn'
  if (s.state === 'connecting' || s.pending > 0) return 'busy'
  return 'ok'
}

/** i18n key (under `settings.sync.state`) for the status label. */
export function syncStateKey(s: SyncStatus | null | undefined): string {
  if (!s || s.state === 'disabled') return 'disabled'
  if (s.paused) return 'paused'
  return s.state
}

/** Tauri's error for a command this build does not register. */
export function isMissingCommand(e: unknown): boolean {
  return /command \S+ not found/i.test(String(e))
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

/**
 * Flatten a bootstrap preview / report into `[label, count]` rows. The shape
 * belongs to the bootstrap command, so accept any nesting of numeric fields
 * (`{ counts: { notes: 3 } }`, `{ notes: 3 }`, `{ remote: { notes: 3 } }`) and
 * drop zero counts.
 */
export function countRows(value: unknown, prefix = ''): Array<[string, number]> {
  if (!value || typeof value !== 'object') return []
  const out: Array<[string, number]> = []
  for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
    const key = k === 'counts' ? prefix : prefix ? `${prefix}.${k}` : k
    if (typeof v === 'number') {
      if (v !== 0) out.push([key || k, v])
    } else if (v && typeof v === 'object' && !Array.isArray(v)) {
      out.push(...countRows(v, key))
    }
  }
  return out
}

/** Projects the group knows that have no folder on this machine yet. */
export function unmappedProjects(list: ProjectMapping[]): ProjectMapping[] {
  return list.filter((p) => !p.local_path?.trim())
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
