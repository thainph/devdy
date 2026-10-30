// Typed wrappers for the two-way sync commands (src-tauri/src/commands/{sync,sync_bootstrap}.rs)
// and the `sync://*` events.
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { invoke } from '@/lib/tauri'
import type {
  BootstrapPreview, BootstrapReport, JoinRequest, ParkedRow, ProjectMapping, SyncDevice, SyncStatus,
} from '@/lib/syncCore'

export * from '@/lib/syncCore'

export const SYNC_STATUS_EVENT = 'sync://status'
/** Synced DB rows were applied; payload `{ tables: string[] }`. */
export const SYNC_ROWS_APPLIED_EVENT = 'sync://rows-applied'
/** Host side: a device asks to join (needs Allow / Deny). */
export const SYNC_JOIN_REQUEST_EVENT = 'sync://join-request'
/** Host side: a join request was answered, timed out or withdrawn. */
export const SYNC_JOIN_RESOLVED_EVENT = 'sync://join-resolved'
/** Host side: the "accept a new device" window opened / closed; `{ expires_at }`. */
export const SYNC_ACCEPT_WINDOW_EVENT = 'sync://accept-window'

export const syncStatus = () => invoke<SyncStatus>('sync_status')
export const syncCreateGroup = () => invoke<SyncStatus>('sync_create_group')
/** This machine has its own relay URL + token (needed to create / join). */
export const syncRelayReady = () => invoke<boolean>('sync_relay_ready')
/** Joins without syncing: preview + commit the first-join merge next. */
export const syncJoin = (deviceAlias: string, password: string) =>
  invoke<SyncStatus>('sync_join', { deviceAlias, password })
/**
 * Resolves to whether the relay confirmed the revoke. A leader / creator whose role hand-off could
 * not be pushed is refused with `sync_leave_handoff_pending` unless `force`.
 */
export const syncLeave = (force = false) => invoke<boolean>('sync_leave', { force })
export const syncPause = (paused: boolean) => invoke<SyncStatus>('sync_pause', { paused })
export const syncNow = () => invoke<void>('sync_now')
export const syncGetSecrets = () => invoke<boolean>('sync_get_secrets')
export const syncSetSecrets = (enabled: boolean) => invoke<void>('sync_set_secrets', { enabled })
/** Rows the relay permanently refused, newest first. */
export const syncParkedList = () => invoke<ParkedRow[]>('sync_parked_list')
/** Queue the parked rows for push again; resolves to the count. */
export const syncParkedRetry = () => invoke<number>('sync_parked_retry')

export const syncDevicesList = () => invoke<SyncDevice[]>('sync_devices_list')
export const syncDeviceRename = (name: string) => invoke<void>('sync_device_rename', { name })
export const syncRevokeDevice = (deviceId: string) => invoke<void>('sync_revoke_device', { deviceId })
/** Open "Accept a new device"; resolves to its expiry (unix millis). */
export const syncAcceptOpen = () => invoke<number>('sync_accept_open')
export const syncAcceptClose = () => invoke<void>('sync_accept_close')
export const syncAcceptState = () => invoke<number | null>('sync_accept_state')
export const syncJoinRespond = (requestId: string, allow: boolean) =>
  invoke<void>('sync_join_respond', { requestId, allow })
/** `null` clears the leader: every device runs the singleton jobs. */
export const syncSetLeader = (deviceId: string | null) => invoke<void>('sync_set_leader', { deviceId })

/** Queue every synced row for push; resolves to the count. */
export const syncResyncAll = () => invoke<number>('sync_resync_all')
/** Joined a group but the first-join merge is not committed yet (engine stopped). */
export const syncBootstrapPending = () => invoke<boolean>('sync_bootstrap_pending')
export const syncBootstrapPreview = () => invoke<BootstrapPreview>('sync_bootstrap_preview')
export const syncBootstrapCommit = () => invoke<BootstrapReport>('sync_bootstrap_commit')
/** The relay lost ops this device already pulled (`relay_behind`): stop and merge again. */
export const syncRebootstrap = () => invoke<void>('sync_rebootstrap')
export const syncProjectMappingList = () => invoke<ProjectMapping[]>('sync_project_mapping_list')
export const syncProjectMappingSet = (projectId: string, localPath: string) =>
  invoke<ProjectMapping>('sync_project_mapping_set', { projectId, localPath })

export function onSyncStatus(cb: (s: SyncStatus) => void): Promise<UnlistenFn> {
  return listen<SyncStatus>(SYNC_STATUS_EVENT, (e) => cb(e.payload))
}

export function onSyncRowsApplied(cb: (tables: string[]) => void): Promise<UnlistenFn> {
  return listen<{ tables?: string[] }>(SYNC_ROWS_APPLIED_EVENT, (e) => cb(e.payload?.tables ?? []))
}

export function onSyncJoinRequest(cb: (r: JoinRequest) => void): Promise<UnlistenFn> {
  return listen<JoinRequest>(SYNC_JOIN_REQUEST_EVENT, (e) => cb(e.payload))
}

export function onSyncJoinResolved(cb: (requestId: string, granted: boolean) => void): Promise<UnlistenFn> {
  return listen<{ request_id: string; granted: boolean }>(SYNC_JOIN_RESOLVED_EVENT, (e) =>
    cb(e.payload.request_id, e.payload.granted),
  )
}

export function onSyncAcceptWindow(cb: (expiresAt: number | null) => void): Promise<UnlistenFn> {
  return listen<{ expires_at: number | null }>(SYNC_ACCEPT_WINDOW_EVENT, (e) => cb(e.payload?.expires_at ?? null))
}
