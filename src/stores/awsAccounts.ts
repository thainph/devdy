import { defineStore } from 'pinia'
import { invoke } from '@/lib/tauri'
import { ref } from 'vue'

// All AWS accounts are profile/SSO based.
export type AwsAuthMethod = 'profile'

// Sentinel returned by validate_aws_account when a profile's SSO token is
// missing or expired and the user needs to run `aws sso login` again.
export const SSO_LOGIN_REQUIRED = 'SSO_LOGIN_REQUIRED'

/** True when a validate error means the SSO token must be refreshed. */
export function isSsoLoginRequired(error: unknown): boolean {
  return String(error).includes(SSO_LOGIN_REQUIRED)
}

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
  region?: string
  profileName?: string
  tags?: string
}

/** A CLI profile discovered in ~/.aws/config. */
export interface AwsProfileInfo {
  name: string
  ssoRoleName: string | null
  ssoAccountId: string | null
  ssoSession: string | null
  region: string | null
  isSso: boolean
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

  async function ssoLogin(id: string): Promise<void> {
    await invoke('aws_sso_login', { id })
  }

  /** List profiles found in ~/.aws/config. */
  async function listProfiles(): Promise<AwsProfileInfo[]> {
    return invoke<AwsProfileInfo[]>('list_aws_profiles')
  }

  /** Refresh an expired SSO token for a profile (browser flow). */
  async function ssoLoginProfile(profileName: string): Promise<void> {
    await invoke('aws_sso_login_profile', { profileName })
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
    ssoLogin,
    listProfiles,
    ssoLoginProfile,
  }
})
