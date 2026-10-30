// Typed wrappers for the two-way sync commands (src-tauri/src/commands/sync.rs) and the
// `sync://status` event. Some commands belong to other sync workers (bootstrap, project
// mapping) and may not exist in this build: `optional()` turns Tauri's "command not found"
// into `MISSING` so the UI can show a disabled state instead of an error.
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { invoke } from '@/lib/tauri'
import { isMissingCommand, type ProjectMapping, type SyncDevice, type SyncStatus } from '@/lib/syncCore'

export * from '@/lib/syncCore'

export const SYNC_STATUS_EVENT = 'sync://status'

/** Returned by an optional command this build does not have. */
export const MISSING = Symbol('missing-command')
export type Optional<T> = T | typeof MISSING

async function optional<T>(cmd: string, args?: Record<string, unknown>): Promise<Optional<T>> {
  try {
    return await invoke<T>(cmd, args)
  } catch (e) {
    if (isMissingCommand(e)) return MISSING
    throw e
  }
}

export const syncStatus = () => invoke<SyncStatus>('sync_status')
export const syncCreateGroup = () => invoke<SyncStatus>('sync_create_group')
export const syncJoin = (deviceAlias: string, password: string) =>
  invoke<SyncStatus>('sync_join', { deviceAlias, password })
/** Resolves to whether the relay confirmed the revoke. */
export const syncLeave = () => invoke<boolean>('sync_leave')
export const syncPause = (paused: boolean) => invoke<SyncStatus>('sync_pause', { paused })
export const syncNow = () => invoke<void>('sync_now')
export const syncGetSecrets = () => optional<boolean>('sync_get_secrets')
export const syncSetSecrets = (enabled: boolean) => invoke<void>('sync_set_secrets', { enabled })

export const syncDevicesList = () => optional<SyncDevice[]>('sync_devices_list')
export const syncDeviceRename = (name: string) => optional<void>('sync_device_rename', { name })
export const syncRevokeDevice = (deviceId: string) => optional<void>('sync_revoke_device', { deviceId })
/** `null` clears the leader: every device runs the singleton jobs. */
export const syncSetLeader = (deviceId: string | null) => optional<void>('sync_set_leader', { deviceId })

export const syncResyncAll = () => optional<unknown>('sync_resync_all')
export const syncBootstrapPreview = () => optional<unknown>('sync_bootstrap_preview')
export const syncBootstrapCommit = () => optional<unknown>('sync_bootstrap_commit')
export const syncProjectMappingList = () => optional<ProjectMapping[]>('sync_project_mapping_list')
export const syncProjectMappingSet = (remoteProjectId: string, localPath: string) =>
  optional<void>('sync_project_mapping_set', { remoteProjectId, localPath })

export function onSyncStatus(cb: (s: SyncStatus) => void): Promise<UnlistenFn> {
  return listen<SyncStatus>(SYNC_STATUS_EVENT, (e) => cb(e.payload))
}
