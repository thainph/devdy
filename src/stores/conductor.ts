/**
 * Conductor store — thin wrapper over the Tauri conductor commands plus the
 * currently-selected session and a light poller for its live detail (workers +
 * event timeline). NO orchestration logic lives here: the conductor *run itself*
 * drives its workers via MCP. This store only launches sessions and reads their
 * state back for display.
 *
 * A conductor IS an ordinary session run (role='conductor'), so it lives in the
 * normal run viewer; this store just adds the worker-tree/timeline it needs.
 *
 * Backend command args are camelCase (Tauri auto-maps to snake_case); return
 * values are plain snake_case (the Rust structs have no rename_all).
 */
import { defineStore } from 'pinia'
import { ref } from 'vue'
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

  let pollTimer: ReturnType<typeof setInterval> | null = null

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

  async function refreshDetail(): Promise<void> {
    if (!selectedId.value) return
    try {
      detail.value = await invoke<ConductorDetail>('get_conductor_detail', {
        conductorRunId: selectedId.value,
      })
    } catch {
      /* transient — keep last snapshot */
    }
  }

  /** Point the store at the run currently open in the viewer. If it turns out to
   * be a conductor (detail.session != null) we keep polling its worker tree;
   * otherwise we clear so the viewer shows a plain session. */
  async function select(runId: string | null): Promise<void> {
    stopPolling()
    selectedId.value = runId
    detail.value = null
    if (!runId) return
    await refreshDetail()
    // Only keep polling when this run is actually a conductor.
    if ((detail.value as ConductorDetail | null)?.session) {
      pollTimer = setInterval(() => void refreshDetail(), 2000)
    } else {
      selectedId.value = null
    }
  }

  function stopPolling(): void {
    if (pollTimer) {
      clearInterval(pollTimer)
      pollTimer = null
    }
  }

  return { selectedId, detail, start, select, refreshDetail, stopPolling }
})
