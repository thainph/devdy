import { defineStore } from 'pinia'
import { ref } from 'vue'
import { invoke } from '@/lib/tauri'

/**
 * Reactive mirror of the backend `AppSettings`, loaded once and shared across
 * components that need settings outside of the Settings screen (the context
 * meter and the token-budget badge). SettingsView still owns editing; it calls
 * `refresh()` after persisting so this store stays in sync.
 */
export interface AppSettings {
  default_engine: string
  claude_path: string
  codex_path: string
  claude_model: string
  codex_model: string
  extra_args: string
  theme: string
  color_theme: string
  analyze_issue_prompt: string
  review_pr_prompt: string
  default_permission_mode: string
  terminal_app: string
  context_warn_percent: string
  context_limit_override: string
  token_budget_period: string
  token_budget_limit: string
  budget_warn_percent: string
}

export const useAppSettingsStore = defineStore('appSettings', () => {
  const settings = ref<AppSettings | null>(null)
  const loaded = ref(false)

  async function refresh() {
    settings.value = await invoke<AppSettings>('get_settings')
    loaded.value = true
  }

  /** Load once; subsequent calls are no-ops (use refresh() to force-reload). */
  async function ensureLoaded() {
    if (!loaded.value) await refresh()
  }

  return { settings, loaded, refresh, ensureLoaded }
})
