import { defineStore } from 'pinia'
import { ref } from 'vue'
import { invoke } from '@/lib/tauri'
import type { SelectOption } from '@/lib/engineOptions'

/**
 * Dynamically-discovered model catalog. Claude models are fetched from the
 * signed-in account via the backend `list_claude_models` command (which bridges
 * the Agent SDK's `supportedModels()`), so newly released models surface in the
 * UI without a hardcode bump. Codex has no model-list API, so it stays curated.
 *
 * The fetched list AUGMENTS the curated alias options — it never replaces them,
 * so the `opus`/`sonnet`/`haiku` aliases and the `[1m]` 1M-context variants (and
 * the context-limit math that depends on them) keep working.
 */
interface FetchedModel {
  value: string
  label: string
  description?: string
}

export const useModelCatalogStore = defineStore('modelCatalog', () => {
  const claudeDynamic = ref<FetchedModel[]>([])
  const loading = ref(false)
  const loaded = ref(false)
  const error = ref<string | null>(null)

  /** Fetch (once, then cached). Pass `force` to re-fetch on demand. */
  async function fetchClaude(force = false) {
    if (loading.value) return
    if (loaded.value && !force) return
    loading.value = true
    error.value = null
    try {
      claudeDynamic.value = await invoke<FetchedModel[]>('list_claude_models')
      loaded.value = true
    } catch (e) {
      error.value = String(e)
    } finally {
      loading.value = false
    }
  }

  /**
   * Merge curated base options with dynamically-discovered models that aren't
   * already covered. A dynamic id is considered "already covered" when its id is
   * present verbatim, or when it belongs to a family an alias already represents
   * (e.g. base alias `opus` covers `claude-opus-4-5`). Only genuinely new
   * families are appended.
   */
  function mergedClaudeOptions(base: SelectOption[]): SelectOption[] {
    const knownValues = new Set(base.map((o) => o.value))
    const aliasStems = base
      .map((o) => o.value.replace(/\[.*\]$/, '').toLowerCase())
      .filter(Boolean)
    const extras = claudeDynamic.value
      .filter((m) => {
        if (knownValues.has(m.value)) return false
        const id = m.value.toLowerCase()
        return !aliasStems.some((stem) => id.includes(stem))
      })
      .map((m) => ({ value: m.value, label: m.label }))
    return extras.length ? [...base, ...extras] : base
  }

  return { claudeDynamic, loading, loaded, error, fetchClaude, mergedClaudeOptions }
})
