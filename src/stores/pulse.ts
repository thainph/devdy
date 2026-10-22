import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
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

  let unlisten: UnlistenFn[] = []
  let started = false
  let reloadTimer: ReturnType<typeof setTimeout> | null = null

  async function load() {
    loading.value = true
    try {
      projects.value = await invoke<ProjectPulse[]>('pulse_overview', { projectIds: null })
      error.value = null
      loaded.value = true
    } catch (e) {
      error.value = String(e)
    } finally {
      loading.value = false
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

  /** Load once and keep in sync with backend events (idempotent). */
  async function start() {
    if (started) return
    started = true
    const offs = await Promise.all([
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
    unlisten = offs
    await load()
  }

  function stop() {
    unlisten.forEach((f) => f())
    unlisten = []
    started = false
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
    stop,
    refresh,
    watch,
    unwatch,
    move,
    setScreenOpen,
  }
})
