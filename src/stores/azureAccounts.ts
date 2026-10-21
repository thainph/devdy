import { defineStore } from 'pinia'
import { invoke } from '@/lib/tauri'
import { ref } from 'vue'

// Sentinel returned by the Azure commands when the account's isolated az login
// is missing or expired and the user needs to sign in again (`azure_login`).
export const AZURE_LOGIN_REQUIRED = 'AZURE_LOGIN_REQUIRED'

/** True when an error means the account's az login must be refreshed. */
export function isAzureLoginRequired(error: unknown): boolean {
  return String(error).includes(AZURE_LOGIN_REQUIRED)
}

export interface AzureAccount {
  id: string
  label: string
  /** Tenant as entered by the user (id or domain). */
  tenant: string | null
  /** Resolved tenant id after sign-in. */
  tenant_id: string | null
  subscription_id: string | null
  subscription_name: string | null
  user_name: string | null
  tags: string | null
  last_validated_at: string | null
  created_at: string
  logged_in: boolean
}

export interface AzureValidation {
  tenant_id: string
  subscription_id: string
  subscription_name: string
  user_name: string
}

export interface AzureSubscription {
  id: string
  name: string
  tenant_id: string
  is_default: boolean
}

export interface AzureAccountCreatePayload {
  label: string
  tenant?: string
  subscription?: string
  tags?: string
}

export interface AzureAccountUpdatePayload {
  label: string
  subscription?: string
  tags?: string
}

export const useAzureAccountsStore = defineStore('azureAccounts', () => {
  const accounts = ref<AzureAccount[]>([])
  const loading = ref(false)
  const error = ref<string | null>(null)

  async function fetch() {
    loading.value = true
    error.value = null
    try {
      accounts.value = await invoke<AzureAccount[]>('list_azure_accounts')
    } catch (e) {
      error.value = String(e)
    } finally {
      loading.value = false
    }
  }

  /** Creates the account and runs its browser `az login`; blocks until sign-in finishes. */
  async function create(payload: AzureAccountCreatePayload): Promise<AzureAccount> {
    try {
      return await invoke<AzureAccount>('create_azure_account', { payload })
    } finally {
      // Refresh even on failure: the account row may exist while sign-in failed.
      await fetch()
    }
  }

  async function update(id: string, payload: AzureAccountUpdatePayload): Promise<AzureAccount> {
    const account = await invoke<AzureAccount>('update_azure_account', {
      payload: { id, ...payload },
    })
    await fetch()
    return account
  }

  async function remove(id: string): Promise<void> {
    await invoke('delete_azure_account', { id })
    await fetch()
  }

  async function validate(id: string): Promise<AzureValidation> {
    try {
      return await invoke<AzureValidation>('validate_azure_account', { id })
    } finally {
      // Keeps last_validated_at / logged_in / subscription in sync.
      await fetch()
    }
  }

  /** Re-run the account's browser `az login` (long-running). */
  async function login(id: string): Promise<void> {
    await invoke('azure_login', { id })
    await fetch()
  }

  /** Subscriptions visible to the account's signed-in identity. */
  async function listSubscriptions(id: string): Promise<AzureSubscription[]> {
    return invoke<AzureSubscription[]>('list_azure_subscriptions', { id })
  }

  return {
    accounts,
    loading,
    error,
    fetch,
    create,
    update,
    remove,
    validate,
    login,
    listSubscriptions,
  }
})
