// Typed wrappers for the two-way sync commands (src-tauri/src/commands/{sync,sync_bootstrap}.rs)
// and the `sync://*` events.
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { invoke } from '@/lib/tauri'
import type { BootstrapPreview, BootstrapReport, ProjectMapping, SyncDevice, SyncStatus } from '@/lib/syncCore'

export * from '@/lib/syncCore'

export const SYNC_STATUS_EVENT = 'sync://status'
/** Synced DB rows were applied; payload `{ tables: string[] }`. */
export const SYNC_ROWS_APPLIED_EVENT = 'sync://rows-applied'

export const syncStatus = () => invoke<SyncStatus>('sync_status')
export const syncCreateGroup = () => invoke<SyncStatus>('sync_create_group')
/** Joins without syncing: preview + commit the first-join merge next. */
export const syncJoin = (deviceAlias: string, password: string) =>
  invoke<SyncStatus>('sync_join', { deviceAlias, password })
/** Resolves to whether the relay confirmed the revoke. */
export const syncLeave = () => invoke<boolean>('sync_leave')
export const syncPause = (paused: boolean) => invoke<SyncStatus>('sync_pause', { paused })
export const syncNow = () => invoke<void>('sync_now')
export const syncGetSecrets = () => invoke<boolean>('sync_get_secrets')
export const syncSetSecrets = (enabled: boolean) => invoke<void>('sync_set_secrets', { enabled })

export const syncDevicesList = () => invoke<SyncDevice[]>('sync_devices_list')
export const syncDeviceRename = (name: string) => invoke<void>('sync_device_rename', { name })
export const syncRevokeDevice = (deviceId: string) => invoke<void>('sync_revoke_device', { deviceId })
/** `null` clears the leader: every device runs the singleton jobs. */
export const syncSetLeader = (deviceId: string | null) => invoke<void>('sync_set_leader', { deviceId })

/** Queue every synced row for push; resolves to the count. */
export const syncResyncAll = () => invoke<number>('sync_resync_all')
/** Joined a group but the first-join merge is not committed yet (engine stopped). */
export const syncBootstrapPending = () => invoke<boolean>('sync_bootstrap_pending')
export const syncBootstrapPreview = () => invoke<BootstrapPreview>('sync_bootstrap_preview')
export const syncBootstrapCommit = () => invoke<BootstrapReport>('sync_bootstrap_commit')
export const syncProjectMappingList = () => invoke<ProjectMapping[]>('sync_project_mapping_list')
export const syncProjectMappingSet = (projectId: string, localPath: string) =>
  invoke<ProjectMapping>('sync_project_mapping_set', { projectId, localPath })

export function onSyncStatus(cb: (s: SyncStatus) => void): Promise<UnlistenFn> {
  return listen<SyncStatus>(SYNC_STATUS_EVENT, (e) => cb(e.payload))
}

export function onSyncRowsApplied(cb: (tables: string[]) => void): Promise<UnlistenFn> {
  return listen<{ tables?: string[] }>(SYNC_ROWS_APPLIED_EVENT, (e) => cb(e.payload?.tables ?? []))
}
