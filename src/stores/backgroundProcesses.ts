import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { invoke } from '@/lib/tauri'
import {
  CMD_KILL,
  CMD_LIST,
  CMD_RESOLVE_QUIT,
  CMD_STOP,
  EMPTY_COUNTS,
  EVT_CHANGED,
  strayCount,
  type BgCounts,
  type BgSnapshot,
  type QuitAction,
} from '@/lib/backgroundProcesses'

// The startup orphan prompt shows at most once per app launch. Module state (not
// component state) so a component HMR swap or a remount can't bring it back.
let orphanPromptClaimed = false

/** True the first time only; the caller then owns the one startup prompt. */
export function claimOrphanPrompt(): boolean {
  if (orphanPromptClaimed) return false
  orphanPromptClaimed = true
  return true
}

/**
 * Processes Devdy is tracking (sidecars, agent-started dev servers, leftovers
 * from a previous session). Started once by the main window (App.vue): the
 * backend sampler pushes `procs:changed` with fresh counts, which is all the
 * sidebar row and the tray need. The full snapshot (`list_background_processes`)
 * is only fetched while someone looks at it — the Processes screen polls it via
 * `watch()` — plus once at startup for the orphan prompt.
 */
export const useBackgroundProcessesStore = defineStore('backgroundProcesses', () => {
  const counts = ref<BgCounts>({ ...EMPTY_COUNTS })
  const snapshot = ref<BgSnapshot | null>(null)
  const loading = ref(false)
  const error = ref<string | null>(null)

  /** Leftover + orphan: processes nobody owns any more. */
  const stray = computed(() => strayCount(counts.value))

  let unlisten: UnlistenFn | null = null
  let starting: Promise<void> | null = null
  let inflight: Promise<void> | null = null
  // Screens currently showing the snapshot; `procs:changed` refetches it only then.
  let watchers = 0

  async function refresh(): Promise<void> {
    // Coalesce: a timer tick and a change event landing together share one `ps`.
    if (inflight) return inflight
    inflight = (async () => {
      loading.value = snapshot.value === null
      try {
        const snap = await invoke<BgSnapshot>(CMD_LIST)
        snapshot.value = snap
        counts.value = snap.counts
        error.value = null
      } catch (e) {
        error.value = String(e)
      } finally {
        loading.value = false
        inflight = null
      }
    })()
    return inflight
  }

  /** Subscribe to `procs:changed` and take the first snapshot. Idempotent. */
  function start(): Promise<void> {
    if (!starting) {
      starting = (async () => {
        try {
          unlisten = await listen<BgCounts>(EVT_CHANGED, (e) => {
            if (e.payload) counts.value = e.payload
            if (watchers > 0) void refresh()
          })
        } catch {
          // Outside the Tauri shell (browser dev).
        }
        await refresh()
      })()
    }
    return starting
  }

  function stop() {
    unlisten?.()
    unlisten = null
    starting = null
  }

  /**
   * Keep the snapshot fresh while a screen shows it: refresh now, then every
   * `intervalMs`. Returns the function that ends the watch.
   */
  function watch(intervalMs = 3000): () => void {
    watchers++
    void refresh()
    const timer = setInterval(() => void refresh(), intervalMs)
    let done = false
    return () => {
      if (done) return
      done = true
      watchers--
      clearInterval(timer)
    }
  }

  /** Stop whole entries (graceful unless `force`). Returns processes signalled. */
  async function stopEntries(ids: string[], force = false): Promise<number> {
    if (!ids.length) return 0
    try {
      return await invoke<number>(CMD_STOP, { ids, force })
    } finally {
      void refresh()
    }
  }

  /** Stop one process of a tracked entry. */
  async function killProcess(pid: number, force = false): Promise<void> {
    try {
      await invoke(CMD_KILL, { pid, force })
    } finally {
      void refresh()
    }
  }

  /** Answer the backend's quit prompt (`app:quit-requested`). */
  async function resolveQuit(action: QuitAction, remember: boolean): Promise<void> {
    await invoke(CMD_RESOLVE_QUIT, { action, remember })
  }

  return {
    counts,
    snapshot,
    loading,
    error,
    stray,
    start,
    stop,
    refresh,
    watch,
    stopEntries,
    killProcess,
    resolveQuit,
  }
})
