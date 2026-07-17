// AI work-summary state, kept OUTSIDE any component so the in-flight summary
// (and its streamed output) survives navigating away from the Work Digest view
// and back. The backend streams progress via `work_summary:*` Tauri events; the
// raw stream-json messages are folded into `StreamEntry[]` with the same
// `applyStreamEvent` the project "AI result" view uses, so tool activity
// (Read/Grep of the transcript files) renders identically.
import { defineStore } from 'pinia'
import { ref } from 'vue'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { invoke } from '@/lib/tauri'
import { applyStreamEvent, type StreamEntry } from '@/lib/streamEvents'
import type { WorkDigestFilter } from './workDigest'

type Status = 'idle' | 'running' | 'done' | 'error'

/** Stable signature so a streamed summary can be matched to the filter it ran for. */
function filterKey(f: WorkDigestFilter): string {
  return JSON.stringify({
    from: f.from ?? null,
    to: f.to ?? null,
    ids: [...(f.project_ids ?? [])].sort(),
  })
}

export const useWorkSummaryStore = defineStore('workSummary', () => {
  const status = ref<Status>('idle')
  const entries = ref<StreamEntry[]>([])
  /** The filter signature the current entries/status belong to. */
  const key = ref<string | null>(null)

  // Mutable working buffers fed to applyStreamEvent; `entries` mirrors them.
  let working: StreamEntry[] = []
  let toolIndex = new Map<string, number>()

  let unlisteners: UnlistenFn[] = []
  let bound = false

  function commit() {
    entries.value = working.slice()
  }

  async function bind() {
    if (bound) return
    bound = true
    unlisteners.push(
      await listen<{ text: string }>('work_summary:user', (e) => {
        if (status.value !== 'running') return
        working.push({ kind: 'user', text: e.payload.text })
        commit()
      }),
    )
    unlisteners.push(
      await listen<unknown>('work_summary:event', (e) => {
        if (status.value !== 'running') return
        applyStreamEvent({ entries: working, toolIndex }, e.payload)
        commit()
      }),
    )
    unlisteners.push(
      await listen('work_summary:done', () => {
        if (status.value !== 'running') return
        status.value = 'done'
      }),
    )
    unlisteners.push(
      await listen<{ error: string }>('work_summary:error', (e) => {
        if (status.value !== 'running') return
        working.push({ kind: 'error', text: e.payload.error })
        commit()
        status.value = 'error'
      }),
    )
  }

  async function start(filter: WorkDigestFilter) {
    await bind()
    key.value = filterKey(filter)
    working = []
    toolIndex = new Map()
    entries.value = []
    status.value = 'running'
    try {
      // Returns immediately; progress arrives via events.
      await invoke('summarize_work_digest', { filter })
    } catch (e) {
      // Selection/spawn errors surface synchronously.
      working.push({ kind: 'error', text: String(e) })
      commit()
      status.value = 'error'
    }
  }

  async function cancel() {
    try {
      await invoke('cancel_work_summary')
    } catch {
      /* ignore */
    }
    status.value = 'idle'
    working = []
    toolIndex = new Map()
    entries.value = []
    key.value = null
  }

  /** Whether the stored summary belongs to this filter. */
  function isFor(filter: WorkDigestFilter): boolean {
    return key.value === filterKey(filter)
  }

  function dispose() {
    unlisteners.forEach((u) => u())
    unlisteners = []
    bound = false
  }

  return { status, entries, key, start, cancel, isFor, bind, dispose }
})
