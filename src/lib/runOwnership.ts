// Cross-device sessions: which runs another device owns, and whether it is
// running them right now (lease valid). Backend: src-tauri/src/sync/sessions.rs.

/** A run owned by another device (`sync_run_owners`). */
export interface RemoteRunOwner {
  run_id: string
  owner_device: string
  device_name: string | null
  /** RFC 3339; null when the owner is idle. */
  lease_until: string | null
  /** Lease live when the backend answered (measured on this machine's clock). */
  running: boolean
  /** How long the lease stayed live from the backend's answer; null when not running. */
  lease_remaining_ms: number | null
  /** Local time the answer arrived (stamped by the store, see `stampReceived`). */
  received_at_ms?: number
  git_head: string | null
  git_dirty: boolean | null
}

export interface GitState {
  head: string | null
  dirty: boolean | null
}

/** Bytes of the owner's final log + transcript that arrived here so far. */
export interface SyncGap {
  have: number
  want: number
}

/** `sync_take_over_run` result. */
export interface TakeOverResult {
  taken: boolean
  mismatch: { recorded: GitState; local: GitState } | null
  /** The owner's final output is still syncing: retry, or continue anyway. */
  syncing: SyncGap | null
  transcript_available: boolean
}

/** Emitted when synced bytes were appended to a run's log; payload `{ run_id }`. */
export const RUN_LOG_UPDATED_EVENT = 'sync://run-log-updated'

/**
 * Another device took over a run live here and the local sidecar was stopped.
 * Payload `{ run_id, device_name }`.
 */
export const RUN_HANDED_OFF_EVENT = 'sync://run-handed-off'

export type RunOwnership = 'local' | 'remote-running' | 'remote-idle'

/** Record when the owners list arrived, so leases count down locally. */
export function stampReceived(owners: RemoteRunOwner[], nowMs: number): RemoteRunOwner[] {
  return owners.map((o) => ({ ...o, received_at_ms: nowMs }))
}

/**
 * Whether the owner's lease is still live at `nowMs`. Lease timestamps from
 * another machine are never compared with this clock: the backend measures the
 * remaining time locally and it counts down from when the answer arrived.
 */
export function leaseLive(owner: RemoteRunOwner, nowMs: number): boolean {
  if (!owner.running) return false
  if (owner.lease_remaining_ms == null || owner.received_at_ms == null) return true
  return nowMs < owner.received_at_ms + owner.lease_remaining_ms
}

/**
 * Local = not owned elsewhere (the composer works normally). Remote-running =
 * read-only live view. Remote-idle = "Continue here" is offered. The lease
 * counts down against `nowMs`, so a crashed owner's run turns idle on its own.
 */
export function runOwnership(owner: RemoteRunOwner | null | undefined, nowMs: number): RunOwnership {
  if (!owner) return 'local'
  return leaseLive(owner, nowMs) ? 'remote-running' : 'remote-idle'
}

/** Share of the owner's final output that arrived here, 0–100. */
export function syncPercent(gap: SyncGap): number {
  if (gap.want <= 0) return 100
  return Math.max(0, Math.min(100, Math.floor((gap.have * 100) / gap.want)))
}

/** Name shown for the owning device. */
export function ownerLabel(owner: RemoteRunOwner, fallback: string): string {
  return owner.device_name?.trim() || fallback
}

/** Short commit id for the git-mismatch dialog; `fallback` when unknown. */
export function shortHead(head: string | null | undefined, fallback: string): string {
  return head ? head.slice(0, 8) : fallback
}
