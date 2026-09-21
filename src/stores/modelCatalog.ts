import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { invoke } from '@/lib/tauri'
import {
  CLAUDE_MODEL_IDS,
  codexOptions as mergeCodex,
  type SelectOption,
} from '@/lib/engineOptions'

/**
 * Model catalog state, persisted by the backend in the `settings` table and
 * keyed by account. Screens hydrate it via `get_model_caches` — nothing is
 * discovered or probed on open, so launching the app / Run screen / composer
 * spawns no process.
 *
 * The two engines work differently on purpose:
 *
 * - **Codex** has a real catalog (`codex debug models`), so its list is fetched
 *   and REPLACES the curated fallback (keeping only the leading "Default").
 * - **Claude** has no catalog worth reading — `supportedModels()` reports only
 *   the handful of rows the CLI picker shows, while `--model` accepts many more.
 *   Its list is therefore curated in `@/lib/engineOptions`, and what we persist
 *   here is a VALIDATION report: which pinned ids the account still serves.
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

/** One model's verdict from the last validation sweep. */
export interface ModelValidation {
  model: string
  ok: boolean
  error?: string | null
}

export interface ModelValidationReport {
  account_key: string
  account_label: string
  checked_at: string
  results: ModelValidation[]
}

interface ModelCaches {
  codex: Record<string, ModelCacheEntry>
  codex_active_key: string
  claude_validation: Record<string, ModelValidationReport>
  claude_validation_active_key: string
}

function pickEntry<T>(map: Record<string, T>, activeKey: string): T | null {
  return map[activeKey] ?? map.default ?? Object.values(map)[0] ?? null
}

export const useModelCatalogStore = defineStore('modelCatalog', () => {
  const codexByAccount = ref<Record<string, ModelCacheEntry>>({})
  const codexActiveKey = ref('default')
  const codexLoading = ref(false)
  // Fatal (thrown) errors from the refresh command itself, distinct from a
  // discovery failure recorded on the cache entry.
  const codexThrow = ref<string | null>(null)

  const validationByAccount = ref<Record<string, ModelValidationReport>>({})
  const validationActiveKey = ref('default')
  const validating = ref(false)
  const validationThrow = ref<string | null>(null)

  const loaded = ref(false)

  const codexEntry = computed(() => pickEntry(codexByAccount.value, codexActiveKey.value))
  const codexModels = computed<FetchedModel[]>(() => codexEntry.value?.models ?? [])
  const codexError = computed(() => codexThrow.value ?? codexEntry.value?.error ?? null)
  const codexRefreshedAt = computed(() => codexEntry.value?.refreshed_at ?? null)
  const codexAccountLabel = computed(() => codexEntry.value?.account_label ?? '')

  const validation = computed(() =>
    pickEntry(validationByAccount.value, validationActiveKey.value),
  )
  const validationCheckedAt = computed(() => validation.value?.checked_at ?? null)
  const validationAccountLabel = computed(() => validation.value?.account_label ?? '')
  const validationError = computed(() => validationThrow.value)

  /** Verdict per model id, for O(1) lookup while rendering the selector. */
  const validationByModel = computed<Record<string, ModelValidation>>(() => {
    const out: Record<string, ModelValidation> = {}
    for (const r of validation.value?.results ?? []) out[r.model] = r
    return out
  })

  /** Models the last sweep could not reach. Empty when nothing has run yet. */
  const unsupportedModels = computed(() =>
    (validation.value?.results ?? []).filter((r) => !r.ok).map((r) => r.model),
  )

  /** Hydrate from the backend (no discovery, no probing). Loads once. */
  async function ensureLoaded(force = false) {
    if (loaded.value && !force) return
    try {
      const caches = await invoke<ModelCaches>('get_model_caches')
      codexByAccount.value = caches.codex ?? {}
      codexActiveKey.value = caches.codex_active_key || 'default'
      validationByAccount.value = caches.claude_validation ?? {}
      validationActiveKey.value = caches.claude_validation_active_key || 'default'
      loaded.value = true
    } catch {
      // Leave whatever we have; screens fall back to curated lists.
    }
  }

  /**
   * Check every pinned Claude id against the signed-in account and persist the
   * verdicts. Each check is a real one-word turn — the only way to know whether
   * `--model <id>` still works — so this is strictly user-initiated.
   */
  async function validateClaude() {
    if (validating.value) return
    validating.value = true
    validationThrow.value = null
    try {
      const report = await invoke<ModelValidationReport>('validate_claude_models', {
        models: CLAUDE_MODEL_IDS,
      })
      validationByAccount.value = {
        ...validationByAccount.value,
        [report.account_key]: report,
      }
      validationActiveKey.value = report.account_key
      loaded.value = true
    } catch (e) {
      validationThrow.value = String(e)
    } finally {
      validating.value = false
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

  /** Delegates to the shared pure merge in `@/lib/engineOptions`. */
  function codexOptions(base: SelectOption[]): SelectOption[] {
    return mergeCodex(base, codexModels.value)
  }

  /**
   * Annotate a Claude option list with the last sweep's verdicts, so a model the
   * account no longer serves is visibly marked instead of failing at launch.
   * Options are never removed — a stale report must not hide a working model.
   */
  function withValidation(options: SelectOption[]): SelectOption[] {
    const verdicts = validationByModel.value
    if (!Object.keys(verdicts).length) return options
    return options.map((o) => {
      const v = verdicts[o.value]
      if (!v || v.ok) return o
      return { ...o, description: `⚠ Not available · ${v.error ?? 'check failed'}` }
    })
  }

  return {
    // state
    codexByAccount,
    codexActiveKey,
    codexLoading,
    validating,
    loaded,
    // getters
    codexEntry,
    codexModels,
    codexError,
    codexRefreshedAt,
    codexAccountLabel,
    validation,
    validationByModel,
    validationCheckedAt,
    validationAccountLabel,
    validationError,
    unsupportedModels,
    // actions
    ensureLoaded,
    validateClaude,
    refreshCodex,
    codexOptions,
    withValidation,
  }
})
