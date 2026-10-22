import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { listen } from '@tauri-apps/api/event'
import { invoke } from '@/lib/tauri'
import {
  redProjectCount,
  watchIndex,
  type ProjectPulse,
  type PulseProgress,
  type PulseWatch,
} from '@/lib/pulse'

/**
 * Project Pulse overview (`pulse_overview`: stored digests + health, no
 * network) for the Pulse screen and the sidebar badge. Reloads on
 * `pulse:changed`; `pulse:refresh-progress` drives the "Refreshing 4/12" chip.
 * Opening the screen never refreshes sources (G2) — only the Refresh buttons do.
 */
const RELOAD_EVERY_MS = 5 * 60_000

/** Newest `checkedAt` across all watches (ms), or null. */
function lastChecked(projects: ProjectPulse[]): number | null {
  let best: number | null = null
  for (const p of projects) {
    for (const w of p.watches) {
      const t = w.checkedAt ? Date.parse(w.checkedAt) : NaN
      if (Number.isFinite(t) && (best == null || t > best)) best = t
    }
  }
  return best
}

export const usePulseStore = defineStore('pulse', () => {
  const projects = ref<ProjectPulse[]>([])
  const loading = ref(false)
  const loaded = ref(false)
  const error = ref<string | null>(null)
  const progress = ref<PulseProgress | null>(null)
  /** Last time a refresh finished (ms), for "Updated at HH:mm". */
  const updatedAt = ref<number | null>(null)

  const redCount = computed(() => redProjectCount(projects.value))
  const watched = computed(() => watchIndex(projects.value))

  let starting: Promise<void> | null = null
  let reloadTimer: ReturnType<typeof setTimeout> | null = null
  /** Sequence of `load()` calls: an older response never overwrites a newer one. */
  let loadSeq = 0

  async function load() {
    const seq = ++loadSeq
    loading.value = true
    try {
      const data = await invoke<ProjectPulse[]>('pulse_overview', { projectIds: null })
      if (seq !== loadSeq) return
      projects.value = data
      error.value = null
      loaded.value = true
      // "Updated at" starts from the newest check on record.
      if (updatedAt.value == null) updatedAt.value = lastChecked(data)
    } catch (e) {
      if (seq === loadSeq) error.value = String(e)
    } finally {
      if (seq === loadSeq) loading.value = false
    }
  }

  /** Coalesce bursts of `pulse:changed` (one per finished source). */
  function scheduleReload() {
    if (reloadTimer) return
    reloadTimer = setTimeout(() => {
      reloadTimer = null
      void load()
    }, 150)
  }

  /**
   * Load once and keep in sync (idempotent: every caller gets the same
   * promise). Besides backend events, the overview is re-read every 5 minutes
   * and when the window regains focus — stored data only, no network — so
   * time-based health (a ball held > 24h, a deadline passing) stays current.
   * Lives as long as the window; there is no stop.
   */
  function start(): Promise<void> {
    starting ??= doStart()
    return starting
  }

  async function doStart() {
    setInterval(() => void load(), RELOAD_EVERY_MS)
    window.addEventListener('focus', onFocus)
    await Promise.all([
      listen('pulse:changed', scheduleReload),
      listen<PulseProgress>('pulse:refresh-progress', (e) => {
        const p = e.payload
        if (p.total > 0 && p.done >= p.total) {
          progress.value = null
          updatedAt.value = Date.now()
        } else {
          progress.value = p
        }
      }),
    ])
    await load()
  }

  function onFocus() {
    scheduleReload()
  }

  /** Refresh one watch / project / everything; progress arrives as events. */
  async function refresh(scope: { projectId?: string; watchId?: string } = {}) {
    const r = await invoke<{ queued: number }>('pulse_refresh', {
      projectId: scope.projectId ?? null,
      watchId: scope.watchId ?? null,
    })
    if (r.queued === 0) updatedAt.value = Date.now()
    return r.queued
  }

  async function watch(url: string, projectId: string, via: 'button' | 'paste') {
    const w = await invoke<PulseWatch>('pulse_watch', { url, projectId, via })
    scheduleReload()
    return w
  }

  async function unwatch(watchId: string) {
    await invoke('pulse_unwatch', { watchId })
    scheduleReload()
  }

  async function move(watchId: string, projectId: string) {
    const w = await invoke<PulseWatch>('pulse_move_watch', { watchId, projectId })
    scheduleReload()
    return w
  }

  /** The Pulse screen keeps MCP requests polled while open (BR-11). */
  function setScreenOpen(open: boolean) {
    void invoke('pulse_set_screen_open', { open }).catch(() => {})
  }

  return {
    projects,
    loading,
    loaded,
    error,
    progress,
    updatedAt,
    redCount,
    watched,
    load,
    start,
    refresh,
    watch,
    unwatch,
    move,
    setScreenOpen,
  }
})
