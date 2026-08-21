import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { getUsageStats } from '@/stores/stats'
import { mascotLevelFromTokens } from '@/lib/mascotLevel'

/**
 * Singleton source of the mascot's earned level. Reads the ALL-TIME cumulative
 * token total (`get_usage_stats` with an empty filter → summary.total_tokens) and
 * derives the current realm / tier. Refreshed on mount and whenever a run finishes.
 */
export const useMascotLevelStore = defineStore('mascotLevel', () => {
  const totalTokens = ref(0)
  const loaded = ref(false)
  let inflight: Promise<void> | null = null

  async function refresh() {
    // Coalesce concurrent refreshes (both mascot surfaces may ask at once).
    if (inflight) return inflight
    inflight = (async () => {
      try {
        const res = await getUsageStats({})
        totalTokens.value = res?.summary?.total_tokens ?? 0
        loaded.value = true
      } catch {
        /* Non-Tauri shell or no usage yet — the fox simply stays at level 1. */
      } finally {
        inflight = null
      }
    })()
    return inflight
  }

  async function ensureLoaded() {
    if (!loaded.value) await refresh()
  }

  const info = computed(() => mascotLevelFromTokens(totalTokens.value))

  return { totalTokens, loaded, refresh, ensureLoaded, info }
})
