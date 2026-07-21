import { defineStore } from 'pinia'
import { invoke } from '@/lib/tauri'
import { ref } from 'vue'

export type AwsAuthMethod = 'keys' | 'profile'

export interface AwsAccount {
  id: string
  label: string
  auth_method: AwsAuthMethod
  account_id: string | null
  arn: string | null
  region: string
  access_key_id: string | null
  profile_name: string | null
  tags: string | null
  has_secret: boolean
  last_validated_at: string | null
  created_at: string
}

export interface AwsValidation {
  account_id: string
  arn: string
  user_id: string
}

export interface AwsAccountPayload {
  label: string
  authMethod: AwsAuthMethod
  region?: string
  accessKeyId?: string
  secretAccessKey?: string
  sessionToken?: string
  profileName?: string
  tags?: string
}

export const useAwsAccountsStore = defineStore('awsAccounts', () => {
  const accounts = ref<AwsAccount[]>([])
  const loading = ref(false)
  const error = ref<string | null>(null)

  async function fetch() {
    loading.value = true
    error.value = null
    try {
      accounts.value = await invoke<AwsAccount[]>('list_aws_accounts')
    } catch (e) {
      error.value = String(e)
    } finally {
      loading.value = false
    }
  }

  async function create(payload: AwsAccountPayload): Promise<AwsAccount> {
    const account = await invoke<AwsAccount>('create_aws_account', { payload })
    await fetch()
    return account
  }

  async function update(id: string, payload: AwsAccountPayload): Promise<AwsAccount> {
    const account = await invoke<AwsAccount>('update_aws_account', {
      payload: { id, ...payload },
    })
    await fetch()
    return account
  }

  async function remove(id: string): Promise<void> {
    await invoke('delete_aws_account', { id })
    await fetch()
  }

  async function validate(id: string): Promise<AwsValidation> {
    return invoke<AwsValidation>('validate_aws_account', { id })
  }

  return { accounts, loading, error, fetch, create, update, remove, validate }
})
