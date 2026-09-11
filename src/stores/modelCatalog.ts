import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { invoke } from '@/lib/tauri'
import type { SelectOption } from '@/lib/engineOptions'

/**
 * Persisted model catalog. Both Claude and Codex model lists are refreshed
 * MANUALLY (from the Settings screen) and cached in the backend `settings` table
 * keyed by account. Screens hydrate the cache via `get_model_caches` — they never
 * trigger discovery on open, so launching the app / Run screen / composer spawns
 * nothing. When a cache is empty the UI falls back to the curated alias tables.
 *
 * - Claude discovery bridges the Agent SDK's `supportedModels()` (sidecar).
 * - Codex discovery runs `codex debug models` (CLI).
 *
 * The fetched Claude list AUGMENTS the curated aliases (`opus`/`sonnet`/`haiku`
 * and the `[1m]` 1M-context variants stay, along with the context-limit math).
 * The Codex cache, being authoritative from the CLI, REPLACES the curated list
 * (keeping only the leading "Default" option).
 */
export interface FetchedModel {
  value: string
  label: string
  description?: string
}

export interface ModelCacheEntry {
  account_key: string
  account_label: string
  source: string
  refreshed_at?: string | null
  models: FetchedModel[]
  error?: string | null
  error_at?: string | null
}

interface ModelCaches {
  claude: Record<string, ModelCacheEntry>
  codex: Record<string, ModelCacheEntry>
  claude_active_key: string
  codex_active_key: string
}

function pickEntry(
  map: Record<string, ModelCacheEntry>,
  activeKey: string,
): ModelCacheEntry | null {
  return map[activeKey] ?? map.default ?? Object.values(map)[0] ?? null
}

export const useModelCatalogStore = defineStore('modelCatalog', () => {
  const claudeByAccount = ref<Record<string, ModelCacheEntry>>({})
  const codexByAccount = ref<Record<string, ModelCacheEntry>>({})
  const claudeActiveKey = ref('default')
  const codexActiveKey = ref('default')
  const claudeLoading = ref(false)
  const codexLoading = ref(false)
  // Fatal (thrown) errors from the refresh command itself, distinct from a
  // discovery failure recorded on the cache entry.
  const claudeThrow = ref<string | null>(null)
  const codexThrow = ref<string | null>(null)
  const loaded = ref(false)

  const claudeEntry = computed(() => pickEntry(claudeByAccount.value, claudeActiveKey.value))
  const codexEntry = computed(() => pickEntry(codexByAccount.value, codexActiveKey.value))

  const claudeModels = computed<FetchedModel[]>(() => claudeEntry.value?.models ?? [])
  const codexModels = computed<FetchedModel[]>(() => codexEntry.value?.models ?? [])

  const claudeError = computed(() => claudeThrow.value ?? claudeEntry.value?.error ?? null)
  const codexError = computed(() => codexThrow.value ?? codexEntry.value?.error ?? null)
  const claudeRefreshedAt = computed(() => claudeEntry.value?.refreshed_at ?? null)
  const codexRefreshedAt = computed(() => codexEntry.value?.refreshed_at ?? null)
  const claudeAccountLabel = computed(() => claudeEntry.value?.account_label ?? '')
  const codexAccountLabel = computed(() => codexEntry.value?.account_label ?? '')

  /** Hydrate the caches from the backend (no discovery). Loads once. */
  async function ensureLoaded(force = false) {
    if (loaded.value && !force) return
    try {
      const caches = await invoke<ModelCaches>('get_model_caches')
      claudeByAccount.value = caches.claude ?? {}
      codexByAccount.value = caches.codex ?? {}
      claudeActiveKey.value = caches.claude_active_key || 'default'
      codexActiveKey.value = caches.codex_active_key || 'default'
      loaded.value = true
    } catch {
      // Leave whatever we have; screens fall back to curated lists.
    }
  }

  /** Manually refresh Claude models from the account and persist the cache. */
  async function refreshClaude() {
    if (claudeLoading.value) return
    claudeLoading.value = true
    claudeThrow.value = null
    try {
      const entry = await invoke<ModelCacheEntry>('refresh_claude_models')
      claudeByAccount.value = { ...claudeByAccount.value, [entry.account_key]: entry }
      claudeActiveKey.value = entry.account_key
      loaded.value = true
    } catch (e) {
      claudeThrow.value = String(e)
    } finally {
      claudeLoading.value = false
    }
  }

  /** Manually refresh Codex models via `codex debug models` and persist. */
  async function refreshCodex() {
    if (codexLoading.value) return
    codexLoading.value = true
    codexThrow.value = null
    try {
      const entry = await invoke<ModelCacheEntry>('refresh_codex_models')
      codexByAccount.value = { ...codexByAccount.value, [entry.account_key]: entry }
      codexActiveKey.value = entry.account_key
      loaded.value = true
    } catch (e) {
      codexThrow.value = String(e)
    } finally {
      codexLoading.value = false
    }
  }

  /**
   * Merge curated base options with cached Claude models that aren't already
   * covered. A cached id is "already covered" when present verbatim, or when it
   * belongs to a family an alias already represents (e.g. `opus` covers
   * `claude-opus-4-5`). Only genuinely new families are appended.
   */
  function mergedClaudeOptions(base: SelectOption[]): SelectOption[] {
    const knownValues = new Set(base.map((o) => o.value))
    const aliasStems = base
      .map((o) => o.value.replace(/\[.*\]$/, '').toLowerCase())
      .filter(Boolean)
    const extras = claudeModels.value
      .filter((m) => {
        if (knownValues.has(m.value)) return false
        const id = m.value.toLowerCase()
        return !aliasStems.some((stem) => id.includes(stem))
      })
      .map((m) => ({ value: m.value, label: m.label }))
    return extras.length ? [...base, ...extras] : base
  }

  /**
   * Codex options: when a cache exists it is authoritative (from the CLI), so we
   * keep only the leading "Default" option and replace the curated models with
   * the cached list. With no cache we return the curated fallback unchanged.
   */
  function codexOptions(base: SelectOption[]): SelectOption[] {
    const models = codexModels.value
    if (!models.length) return base
    const defaultOpt = base.find((o) => o.value === '') ?? { value: '', label: base[0]?.label ?? '' }
    return [defaultOpt, ...models.map((m) => ({ value: m.value, label: m.label }))]
  }

  return {
    // state
    claudeByAccount,
    codexByAccount,
    claudeActiveKey,
    codexActiveKey,
    claudeLoading,
    codexLoading,
    loaded,
    // getters
    claudeEntry,
    codexEntry,
    claudeModels,
    codexModels,
    claudeError,
    codexError,
    claudeRefreshedAt,
    codexRefreshedAt,
    claudeAccountLabel,
    codexAccountLabel,
    // actions
    ensureLoaded,
    refreshClaude,
    refreshCodex,
    mergedClaudeOptions,
    codexOptions,
  }
})
