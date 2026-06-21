import { defineStore } from 'pinia'
import { invoke } from '@/lib/tauri'
import { ref } from 'vue'

export interface GithubAccount {
  id: string
  label: string
  username: string | null
  scopes: string[]
  has_pat: boolean
  created_at: string
}

export interface PatValidation {
  username: string
  scopes: string[]
  has_repo_scope: boolean
}

export const useGithubAccountsStore = defineStore('githubAccounts', () => {
  const accounts = ref<GithubAccount[]>([])
  const loading = ref(false)
  const error = ref<string | null>(null)

  async function fetch() {
    loading.value = true
    error.value = null
    try {
      accounts.value = await invoke<GithubAccount[]>('list_github_accounts')
    } catch (e) {
      error.value = String(e)
    } finally {
      loading.value = false
    }
  }

  async function create(label: string, pat: string): Promise<GithubAccount> {
    const account = await invoke<GithubAccount>('create_github_account', { label, pat })
    await fetch()
    return account
  }

  async function update(id: string, label: string, pat?: string): Promise<GithubAccount> {
    const account = await invoke<GithubAccount>('update_github_account', {
      id,
      label,
      pat: pat?.trim() ? pat.trim() : null,
    })
    await fetch()
    return account
  }

  async function remove(id: string): Promise<void> {
    await invoke('delete_github_account', { id })
    await fetch()
  }

  async function validate(id: string): Promise<PatValidation> {
    return invoke<PatValidation>('validate_github_account', { id })
  }

  return { accounts, loading, error, fetch, create, update, remove, validate }
})
