// Cross-device sessions: which runs another device owns, and whether it is
// running them right now (lease valid). Backend: src-tauri/src/sync/sessions.rs.

/** A run owned by another device (`sync_run_owners`). */
export interface RemoteRunOwner {
  run_id: string
  owner_device: string
  device_name: string | null
  /** RFC 3339; null when the owner is idle. */
  lease_until: string | null
  /** Lease valid when the backend answered. */
  running: boolean
  git_head: string | null
  git_dirty: boolean | null
}

export interface GitState {
  head: string | null
  dirty: boolean | null
}

/** `sync_take_over_run` result. */
export interface TakeOverResult {
  taken: boolean
  mismatch: { recorded: GitState; local: GitState } | null
  transcript_available: boolean
}

/** Emitted when synced bytes were appended to a run's log; payload `{ run_id }`. */
export const RUN_LOG_UPDATED_EVENT = 'sync://run-log-updated'

export type RunOwnership = 'local' | 'remote-running' | 'remote-idle'

/** Whether a lease timestamp is still in the future at `nowMs`. */
export function leaseActive(leaseUntil: string | null | undefined, nowMs: number): boolean {
  if (!leaseUntil) return false
  const t = Date.parse(leaseUntil)
  return Number.isFinite(t) && t > nowMs
}

/**
 * Local = not owned elsewhere (the composer works normally). Remote-running =
 * read-only live view. Remote-idle = "Continue here" is offered. The lease is
 * re-checked against `nowMs`, so a crashed owner's run turns idle on its own.
 */
export function runOwnership(owner: RemoteRunOwner | null | undefined, nowMs: number): RunOwnership {
  if (!owner) return 'local'
  return leaseActive(owner.lease_until, nowMs) ? 'remote-running' : 'remote-idle'
}

/** Name shown for the owning device. */
export function ownerLabel(owner: RemoteRunOwner, fallback: string): string {
  return owner.device_name?.trim() || fallback
}

/** Short commit id for the git-mismatch dialog; `fallback` when unknown. */
export function shortHead(head: string | null | undefined, fallback: string): string {
  return head ? head.slice(0, 8) : fallback
}
