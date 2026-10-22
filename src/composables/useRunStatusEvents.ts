// Lightweight run-status tracking for a pop-out that only needs "is this run
// still going?" — e.g. the PR Inbox window. The main window's liveRuns store
// attaches a run's full listener set (output, events, permissions) and drives
// app-wide notifications; duplicating that in a pop-out would double-process
// every stream event. Instead this listens to just two backend events, and only
// for the runs the caller cares about:
//
//   • `run:activated` (global) — a tracked run started / resumed → "running"
//   • `run:done:<id>`          — that run finished → its final status
//
// `tracked()` returns the relevant runs with their DB snapshot status (for the
// PR Inbox: the linked review run of each PR, which includes runs started from
// the window once they are linked). A `run:done` subscription is held only
// while a tracked run is believed running (heard status, else snapshot) and is
// dropped as soon as the run is terminal or leaves the tracked set — so a run
// already known finished is never re-subscribed. `status(id)` is the last
// status heard, or undefined (callers fall back to their snapshot).
import { computed, onBeforeUnmount, onMounted, reactive, watch } from 'vue'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

export interface TrackedRun {
  id: string
  /** DB snapshot status (from the caller's last refresh). */
  status: string | null
}

const ACTIVE = new Set(['running', 'fetched'])

export function useRunStatusEvents(tracked: () => TrackedRun[]) {
  const heard = reactive(new Map<string, string>())
  // run id → unlisten (null while the subscription is being set up)
  const doneListeners = new Map<string, UnlistenFn | null>()
  let unlistenActivated: UnlistenFn | null = null
  let disposed = false

  const trackedById = computed(() => new Map(tracked().map(r => [r.id, r.status])))

  // Runs that need a run:done subscription right now.
  const watched = computed(() => {
    const ids = new Set<string>()
    for (const [id, snapshot] of trackedById.value) {
      if (ACTIVE.has(heard.get(id) ?? snapshot ?? '')) ids.add(id)
    }
    return ids
  })

  function unsubscribe(runId: string) {
    doneListeners.get(runId)?.()
    doneListeners.delete(runId)
  }

  async function subscribeDone(runId: string) {
    if (doneListeners.has(runId)) return
    // Reserve synchronously so two watcher ticks can't subscribe twice.
    doneListeners.set(runId, null)
    try {
      const off = await listen<{ run_id: string; status: string }>(`run:done:${runId}`, (e) => {
        heard.set(runId, e.payload?.status ?? 'done')
        unsubscribe(runId)
      })
      // Dropped (or torn down) while the listen() call was in flight.
      if (disposed || !doneListeners.has(runId)) off()
      else doneListeners.set(runId, off)
    } catch {
      doneListeners.delete(runId) // outside the Tauri shell
    }
  }

  watch(
    watched,
    (ids) => {
      ids.forEach((id) => void subscribeDone(id))
      for (const id of [...doneListeners.keys()]) if (!ids.has(id)) unsubscribe(id)
      // Forget heard statuses of runs no longer tracked.
      for (const id of [...heard.keys()]) if (!trackedById.value.has(id)) heard.delete(id)
    },
    { immediate: true },
  )

  onMounted(async () => {
    try {
      unlistenActivated = await listen<{ run_id: string }>('run:activated', (e) => {
        const id = e.payload?.run_id
        // Only runs this view tracks; `watched` then subscribes to its run:done.
        if (id && trackedById.value.has(id)) heard.set(id, 'running')
      })
      if (disposed) unlistenActivated()
    } catch {
      /* outside the Tauri shell */
    }
  })

  onBeforeUnmount(() => {
    disposed = true
    unlistenActivated?.()
    doneListeners.forEach((off) => off?.())
    doneListeners.clear()
  })

  return {
    status: (runId: string): string | undefined => heard.get(runId),
  }
}
