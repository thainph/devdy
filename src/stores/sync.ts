import { defineStore } from 'pinia'
import { ref } from 'vue'
import type { UnlistenFn } from '@tauri-apps/api/event'
import { DISABLED_STATUS, onSyncStatus, syncBootstrapPending, syncStatus, type SyncStatus } from '@/lib/sync'

/**
 * Two-way sync status shared by Settings → Sync and the sidebar indicator. Kept fresh
 * by the `sync://status` event; `refresh()` re-reads it after a command.
 * `mergePending`: joined a group but the first-join merge is not committed, so the
 * engine is stopped (status reads "disabled") until Settings → Sync commits it.
 */
export const useSyncStore = defineStore('sync', () => {
  const status = ref<SyncStatus>({ ...DISABLED_STATUS })
  const mergePending = ref(false)
  let unlisten: UnlistenFn | null = null
  let starting: Promise<void> | null = null

  async function refresh() {
    try {
      const [s, pending] = await Promise.all([syncStatus(), syncBootstrapPending()])
      status.value = s
      mergePending.value = pending
    } catch {
      // A build without sync commands: stay "disabled".
    }
  }

  function set(s: SyncStatus) {
    status.value = s
  }

  function setMergePending(v: boolean) {
    mergePending.value = v
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

  return { status, mergePending, refresh, set, setMergePending, start, stop }
})
