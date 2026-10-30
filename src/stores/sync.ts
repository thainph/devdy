import { defineStore } from 'pinia'
import { ref } from 'vue'
import type { UnlistenFn } from '@tauri-apps/api/event'
import { DISABLED_STATUS, onSyncStatus, syncStatus, type SyncStatus } from '@/lib/sync'

/**
 * Two-way sync status shared by Settings → Sync and the sidebar indicator. Kept fresh
 * by the `sync://status` event; `refresh()` re-reads it after a command.
 */
export const useSyncStore = defineStore('sync', () => {
  const status = ref<SyncStatus>({ ...DISABLED_STATUS })
  let unlisten: UnlistenFn | null = null
  let starting: Promise<void> | null = null

  async function refresh() {
    try {
      status.value = await syncStatus()
    } catch {
      // A build without sync commands: stay "disabled".
    }
  }

  function set(s: SyncStatus) {
    status.value = s
  }

  /** Subscribe once (idempotent) and load the current status. */
  function start(): Promise<void> {
    starting ??= (async () => {
      unlisten = await onSyncStatus(set).catch(() => null)
      await refresh()
    })()
    return starting
  }

  function stop() {
    unlisten?.()
    unlisten = null
    starting = null
  }

  return { status, refresh, set, start, stop }
})
