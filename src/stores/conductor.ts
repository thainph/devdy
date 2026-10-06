/**
 * Conductor store — thin wrapper over the Tauri conductor commands plus the
 * currently-selected session and its live detail (workers + event timeline).
 * Detail is refreshed event-driven: we subscribe to `conductor:changed:{id}`
 * (emitted by the backend on spawn/send/cancel) rather than polling on a timer.
 * Per-worker run status is read live from the `liveRuns` store by consumers.
 * NO orchestration logic lives here: the conductor *run itself* drives its
 * workers via MCP. This store only launches sessions and reads their state back
 * for display.
 *
 * A conductor IS an ordinary session run (role='conductor'), so it lives in the
 * normal run viewer; this store just adds the worker-tree/timeline it needs.
 *
 * Backend command args are camelCase (Tauri auto-maps to snake_case); return
 * values are plain snake_case (the Rust structs have no rename_all).
 */
import { defineStore } from 'pinia'
import { ref } from 'vue'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { invoke } from '@/lib/tauri'

export interface ConductorStarted {
  conductor_run_id: string
  project_id: string
}
export interface ConductorSessionRow {
  id: string
  project_id: string
  goal: string
  status: string
  max_workers: number
  worker_count: number
  created_at: string
  finished_at: string | null
}
export interface ConductorWorker {
  worker_id: string
  title: string | null
  status: string
  engine: string
  model: string | null
  claude_account_id: string | null
}
export interface ConductorEvent {
  ts: string
  kind: string
  payload: unknown
}
export interface ConductorDetail {
  session: ConductorSessionRow | null
  workers: ConductorWorker[]
  events: ConductorEvent[]
}

export const useConductorStore = defineStore('conductor', () => {
  /** The conductor run currently open in the viewer (null when the open run is a
   * normal session). */
  const selectedId = ref<string | null>(null)
  const detail = ref<ConductorDetail | null>(null)

  // Event-driven: we subscribe to `conductor:changed:{id}` (emitted by the backend
  // on spawn/send/cancel) instead of polling. `unlistenChanged` tears that down
  // when we retarget or clear. `refreshScheduled` coalesces bursts (e.g. several
  // workers spawned at once) into a single detail fetch.
  let unlistenChanged: UnlistenFn | null = null
  let refreshScheduled = false
  // Bumped on every select(); lets an in-flight select() detect it was superseded
  // by a newer one while awaiting, and bail without clobbering the newer state.
  let selectToken = 0

  /** Promote an existing fetched session run into a conductor and launch it. */
  async function start(
    runId: string,
    goal: string,
    opts?: { maxWorkers?: number; modelOverride?: string },
  ): Promise<ConductorStarted> {
    return invoke<ConductorStarted>('start_conductor', {
      runId,
      goal,
      maxWorkers: opts?.maxWorkers ?? null,
      modelOverride: opts?.modelOverride ?? null,
    })
  }

  /**
   * Convert an already-started, now-stopped normal session into a conductor.
   * This is a pure in-place upgrade: the backend marks the role and creates the
   * conductor session row + token, and nothing else — it does NOT resume the
   * session or send any turn, so it consumes no tokens. The conductor tools are
   * injected the next time the user resumes the session with their own prompt.
   * Only works on a session that has run before and is not currently running.
   */
  async function convertToConductor(
    runId: string,
    opts?: { maxWorkers?: number },
  ): Promise<ConductorStarted> {
    return invoke<ConductorStarted>('convert_run_to_conductor', {
      runId,
      maxWorkers: opts?.maxWorkers ?? null,
    })
  }

  /**
   * Change the worker cap of a running (or finished) conductor. The backend
   * clamps to [1, 50], persists it, and updates the live cap the next
   * session_spawn enforces; returns the value actually stored. We refresh the
   * detail so the sidebar reflects the new max immediately.
   */
  async function setMaxWorkers(conductorRunId: string, maxWorkers: number): Promise<number> {
    const applied = await invoke<number>('set_conductor_max_workers', {
      conductorRunId,
      maxWorkers,
    })
    await refreshDetail()
    return applied
  }

  async function refreshDetail(): Promise<void> {
    if (!selectedId.value) return
    try {
      const d = await invoke<ConductorDetail>('get_conductor_detail', {
        conductorRunId: selectedId.value,
      })
      detail.value = d
      // Definitive answer that this run is a plain session (no conductor row):
      // stop tracking so the viewer shows it as an ordinary run.
      if (!d.session) {
        stopPolling()
        selectedId.value = null
      }
    } catch {
      /* Transient failure (e.g. SQLite busy while workers write their status).
         Keep the last snapshot — a later `conductor:changed` event or a focus
         refresh recovers. We must not treat this as "not a conductor", or the
         worker panel stays blank while the tray still counts the workers. */
    }
  }

  /** Coalesce a burst of `conductor:changed` events into one fetch on the next
   * microtask — spawning N workers fires N events but needs only one refresh. */
  function scheduleRefresh(): void {
    if (refreshScheduled) return
    refreshScheduled = true
    void Promise.resolve().then(() => {
      refreshScheduled = false
      void refreshDetail()
    })
  }

  /** Point the store at the run currently open in the viewer. Does one probe, and
   * if it's a conductor subscribes to its `conductor:changed` stream (worker
   * membership + timeline). Per-worker status stays live via the `liveRuns` store,
   * so there is no polling here. A transient probe failure keeps the selection so
   * a later event / focus refresh can recover; only a successful "not a conductor"
   * probe clears it. */
  async function select(runId: string | null): Promise<void> {
    stopPolling()
    const token = ++selectToken
    selectedId.value = runId
    detail.value = null
    if (!runId) return
    await refreshDetail()
    // Superseded by a newer select() while awaiting? Leave its state alone.
    if (token !== selectToken) return
    // refreshDetail clears selectedId when the probe proved it's not a conductor.
    if (!selectedId.value) return
    const un = await listen(`conductor:changed:${runId}`, () => scheduleRefresh())
    if (token !== selectToken) {
      // A newer select() (or stopPolling) took over during the await — drop the
      // listener we just made; the newer call owns `unlistenChanged`.
      un()
      return
    }
    unlistenChanged = un
  }

  /** Tear down the event subscription. Kept the historical name so existing call
   * sites (e.g. RunView.onUnmounted) stay valid; there is no timer anymore. */
  function stopPolling(): void {
    // Invalidate any in-flight select() so it won't attach a listener after us.
    selectToken++
    if (unlistenChanged) {
      unlistenChanged()
      unlistenChanged = null
    }
  }

  return { selectedId, detail, start, convertToConductor, select, refreshDetail, setMaxWorkers, stopPolling }
})
