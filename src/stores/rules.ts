import { defineStore } from 'pinia'
import { invoke } from '@/lib/tauri'
import { ref } from 'vue'

export type RuleTarget = 'claude' | 'codex' | 'both'

export interface Rule {
  id: string
  name: string
  description: string
  target: RuleTarget
  source_path: string
  updated_at: string
}

export interface RuleContent {
  rule: Rule
  content: string
}

export const useRulesStore = defineStore('rules', () => {
  const rules = ref<Rule[]>([])
  const loading = ref(false)
  const error = ref<string | null>(null)

  async function fetchRules() {
    loading.value = true
    error.value = null
    try {
      rules.value = await invoke<Rule[]>('list_rules')
    } catch (e) {
      error.value = String(e)
    } finally {
      loading.value = false
    }
  }

  async function getRule(id: string): Promise<RuleContent> {
    return invoke<RuleContent>('get_rule', { id })
  }

  async function createRule(payload: { name: string; description: string; target: RuleTarget; content: string }): Promise<Rule> {
    const rule = await invoke<Rule>('create_rule', { payload })
    await fetchRules()
    return rule
  }

  async function updateRule(payload: { id: string; name: string; description: string; target: RuleTarget; content: string }): Promise<Rule> {
    const rule = await invoke<Rule>('update_rule', { payload })
    await fetchRules()
    return rule
  }

  async function deleteRule(id: string): Promise<void> {
    await invoke('delete_rule', { id })
    await fetchRules()
  }

  async function importRule(srcPath: string): Promise<Rule> {
    const rule = await invoke<Rule>('import_rule', { srcPath })
    await fetchRules()
    return rule
  }

  async function exportRule(id: string, destPath: string): Promise<void> {
    await invoke('export_rule', { id, destPath })
  }

  async function openRulesFolder(): Promise<void> {
    await invoke('open_rules_folder')
  }

  return { rules, loading, error, fetchRules, getRule, createRule, updateRule, deleteRule, importRule, exportRule, openRulesFolder }
})
