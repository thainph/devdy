import { defineStore } from 'pinia'
import { invoke } from '@/lib/tauri'
import { ref } from 'vue'

export interface GitlabAccount {
  id: string
  label: string
  username: string | null
  host: string | null
  email: string | null
  scopes: string[]
  has_pat: boolean
  created_at: string
}

export interface GitlabPatValidation {
  username: string
  email: string | null
  scopes: string[]
}

export const useGitlabAccountsStore = defineStore('gitlabAccounts', () => {
  const accounts = ref<GitlabAccount[]>([])
  const loading = ref(false)
  const error = ref<string | null>(null)

  async function fetch() {
    loading.value = true
    error.value = null
    try {
      accounts.value = await invoke<GitlabAccount[]>('list_gitlab_accounts')
    } catch (e) {
      error.value = String(e)
    } finally {
      loading.value = false
    }
  }

  async function create(
    label: string,
    pat: string,
    host?: string,
    email?: string,
  ): Promise<GitlabAccount> {
    const account = await invoke<GitlabAccount>('create_gitlab_account', {
      label,
      pat,
      host: host?.trim() ? host.trim() : null,
      email: email?.trim() ? email.trim() : null,
    })
    await fetch()
    return account
  }

  async function update(
    id: string,
    label: string,
    pat?: string,
    host?: string,
    email?: string,
  ): Promise<GitlabAccount> {
    const account = await invoke<GitlabAccount>('update_gitlab_account', {
      id,
      label,
      pat: pat?.trim() ? pat.trim() : null,
      host: host?.trim() ? host.trim() : null,
      email: email?.trim() ? email.trim() : null,
    })
    await fetch()
    return account
  }

  async function remove(id: string): Promise<void> {
    await invoke('delete_gitlab_account', { id })
    await fetch()
  }

  async function validate(id: string): Promise<GitlabPatValidation> {
    return invoke<GitlabPatValidation>('validate_gitlab_account', { id })
  }

  return { accounts, loading, error, fetch, create, update, remove, validate }
})
