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
  language: string
  color_theme: string
  animated_background: string
  analyze_issue_prompt: string
  review_pr_prompt: string
  default_permission_mode: string
  terminal_app: string
  context_warn_percent: string
  context_limit_override: string
  budget_5h_percent: string
  budget_week_percent: string
  translate_engine: string
  translate_model: string
  translate_target_lang: string
  translate_style: string
  mcp_builtin_devdy_enabled: string
  cyber_fox_enabled: string
  cyber_fox_size: string
  cyber_fox_mode: string
  cyber_fox_sound: string
  mascot_speech_enabled: string
  mascot_speech_engine: string
  mascot_speech_model: string
  cyber_fox_lite: string
  cyber_fox_voice_vi: string
  cyber_fox_voice_en: string
  cyber_fox_voice_rate: string
  cyber_fox_voice_pitch: string
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
