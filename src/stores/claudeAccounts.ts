import { defineStore } from 'pinia'
import { invoke } from '@/lib/tauri'
import { ref } from 'vue'

export interface ClaudeAccount {
  id: string
  label: string
  config_dir: string
  email: string | null
  auth_method: string | null
  api_provider: string | null
  status: string
  is_default: boolean
  last_checked_at: string | null
  created_at: string
}

export interface ClaudeValidation {
  logged_in: boolean
  status: string
  email: string | null
  auth_method: string | null
  api_provider: string | null
}

export interface AccountBudget {
  source: 'plan' | 'disabled'
  period: string
  percent: number
  is_warning: boolean
  is_over: boolean
  reset: string | null
  captured_at: string | null
  is_stale: boolean
  status: 'allowed' | 'warning' | 'blocked' | null
  rolled_over: boolean
}

export const useClaudeAccountsStore = defineStore('claudeAccounts', () => {
  const accounts = ref<ClaudeAccount[]>([])
  const loading = ref(false)
  const error = ref<string | null>(null)
  // Per-account plan-usage verdict, keyed by account id (populated lazily).
  const budgets = ref<Record<string, AccountBudget>>({})
  // Per-account live-probe state for the usage badge refresh button.
  const refreshingUsage = ref<Record<string, boolean>>({})
  const usageErrors = ref<Record<string, string | null>>({})

  async function fetch() {
    loading.value = true
    error.value = null
    try {
      accounts.value = await invoke<ClaudeAccount[]>('list_claude_accounts')
      await Promise.all(accounts.value.map(a => fetchBudget(a.id)))
    } catch (e) {
      error.value = String(e)
    } finally {
      loading.value = false
    }
  }

  async function fetchBudget(id: string) {
    try {
      budgets.value[id] = await invoke<AccountBudget>('get_claude_account_budget', { accountId: id })
    } catch {
      // Best-effort; leave any prior value.
    }
  }

  async function create(label: string): Promise<ClaudeAccount> {
    const account = await invoke<ClaudeAccount>('create_claude_account', { label })
    await fetch()
    return account
  }

  async function rename(id: string, label: string): Promise<ClaudeAccount> {
    const account = await invoke<ClaudeAccount>('rename_claude_account', { id, label })
    await fetch()
    return account
  }

  async function remove(id: string): Promise<void> {
    await invoke('delete_claude_account', { id })
    await fetch()
  }

  // Live-probe /usage for one account, then re-read its snapshot. Backs the
  // per-account refresh button on the usage badge.
  async function refreshUsage(id: string): Promise<void> {
    if (refreshingUsage.value[id]) return
    refreshingUsage.value[id] = true
    usageErrors.value[id] = null
    try {
      await invoke('refresh_plan_usage', { accountId: id })
    } catch (e) {
      usageErrors.value[id] = String(e)
      // Probe is best-effort; fall back to the latest stored snapshot below.
    } finally {
      refreshingUsage.value[id] = false
      await fetchBudget(id)
    }
  }

  async function setDefault(id: string): Promise<void> {
    await invoke('set_default_claude_account', { id })
    await fetch()
  }

  async function openLogin(id: string, terminalApp?: string): Promise<void> {
    await invoke('open_claude_account_login', { id, terminalApp: terminalApp ?? null })
  }

  async function validate(id: string): Promise<ClaudeValidation> {
    const result = await invoke<ClaudeValidation>('validate_claude_account', { id })
    await fetch()
    return result
  }

  return { accounts, loading, error, budgets, refreshingUsage, usageErrors, fetch, fetchBudget, refreshUsage, create, rename, remove, setDefault, openLogin, validate }
})
