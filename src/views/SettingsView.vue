<script setup lang="ts">
import { ref, onMounted, onUnmounted, computed, watch } from 'vue'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { invoke } from '@/lib/tauri'
import {
  Cpu, Palette, FileText, ShieldAlert, Sparkles, Github, Gitlab, Cloud,
  CheckCircle2, AlertTriangle, Trash2, Plus, Pencil, Gauge, Radio,
  RefreshCw, Loader2, Server, HardDrive,
} from 'lucide-vue-next'
import { Button, Input, Textarea, Card, AppSelect } from '@/components/ui'
import RemoteControlSettings from '@/components/remote/RemoteControlSettings.vue'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { useGithubAccountsStore, type PatValidation } from '@/stores/githubAccounts'
import { useGitlabAccountsStore, type GitlabPatValidation } from '@/stores/gitlabAccounts'
import { useAwsAccountsStore, type AwsAccountPayload, type AwsAuthMethod, type AwsValidation } from '@/stores/awsAccounts'
import { useAppSettingsStore } from '@/stores/appSettings'
import { useBudgetStore } from '@/stores/budget'
import { useModelCatalogStore } from '@/stores/modelCatalog'

const appSettings = useAppSettingsStore()
const budget = useBudgetStore()
const modelCatalog = useModelCatalogStore()
const { confirm } = useConfirm()
const { toast } = useToast()

// The detailed subscription plan-usage breakdown now lives in the Stats view.
// Settings keeps the `plan_usage_updated` listener only to keep the budget badge
// (which derives from the same /usage data) in sync.
let unlistenPlanUsage: UnlistenFn | null = null
let unlistenBudgetStatus: UnlistenFn | null = null

interface AppSettings {
  default_engine: string
  claude_path: string
  codex_path: string
  claude_model: string
  codex_model: string
  extra_args: string
  theme: string
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
}

const settings = ref<AppSettings>({
  default_engine: 'claude',
  claude_path: 'claude',
  codex_path: 'codex',
  claude_model: '',
  codex_model: '',
  extra_args: '',
  theme: 'system',
  color_theme: 'default',
  animated_background: 'true',
  analyze_issue_prompt: '',
  review_pr_prompt: '',
  default_permission_mode: 'default',
  terminal_app: 'terminal',
  context_warn_percent: '80',
  context_limit_override: '',
  budget_5h_percent: '',
  budget_week_percent: '',
  translate_engine: 'claude',
  translate_model: '',
  translate_target_lang: 'vi',
  translate_style: 'natural',
  mcp_builtin_devdy_enabled: 'true',
})

// Claude model choices = curated aliases + any newly-released models discovered
// from the account (Codex has no discovery API, so it stays curated).
const claudeModelOptions = computed(() => modelCatalog.mergedClaudeOptions(CLAUDE_MODEL_OPTIONS))

// Translation model choices follow the chosen translation engine, reusing the
// same option tables (with dynamic Claude models merged in) as the selectors.
const translateModelOptions = computed(() =>
  settings.value.translate_engine === 'codex' ? CODEX_MODEL_OPTIONS : claudeModelOptions.value,
)
// Reset the translation model when switching to an engine that doesn't offer it.
watch(
  () => settings.value.translate_engine,
  () => {
    if (!translateModelOptions.value.some((o) => o.value === settings.value.translate_model)) {
      settings.value.translate_model = ''
    }
  },
)

const TRANSLATE_TARGET_OPTIONS = [
  { value: 'vi', label: 'Tiếng Việt' },
  { value: 'en', label: 'English' },
  { value: 'ja', label: '日本語 (Japanese)' },
  { value: 'zh', label: '中文 (Chinese)' },
  { value: 'ko', label: '한국어 (Korean)' },
]
const TRANSLATE_STYLE_OPTIONS = [
  { value: 'natural', label: 'Natural — thoát ý, tự nhiên' },
  { value: 'literal', label: 'Literal — sát nghĩa' },
  { value: 'technical', label: 'Technical — giữ thuật ngữ/code' },
  { value: 'formal', label: 'Formal — trang trọng' },
  { value: 'casual', label: 'Casual — thân mật' },
]

// `[1m]` selects the 1M-context variant; the bare alias uses the 200K default.
// Aliases (not pinned ids) keep these current as new model versions ship.
const CLAUDE_MODEL_OPTIONS = [
  { value: '', label: 'Default (engine decides)' },
  { value: 'fable', label: 'Fable 5 (1M)' },
  { value: 'opus', label: 'Opus (200K)' },
  { value: 'opus[1m]', label: 'Opus (1M)' },
  { value: 'sonnet', label: 'Sonnet (200K)' },
  { value: 'sonnet[1m]', label: 'Sonnet (1M)' },
  { value: 'haiku', label: 'Haiku' },
]
const CODEX_MODEL_OPTIONS = [
  { value: '', label: 'Default (engine decides)' },
  { value: 'gpt-5.5', label: 'gpt-5.5' },
  { value: 'gpt-5.4', label: 'gpt-5.4' },
  { value: 'gpt-5.3-codex', label: 'gpt-5.3-codex' },
  { value: 'gpt-5.2-codex', label: 'gpt-5.2-codex' },
  { value: 'gpt-5.1-codex-mini', label: 'gpt-5.1-codex-mini' },
]
const loading = ref(true)
// Snapshot of the last persisted settings so the auto-save watcher only
// pushes keys that actually changed.
let lastSaved: AppSettings | null = null
let saveTimer: ReturnType<typeof setTimeout> | null = null

const SECTIONS = [
  { id: 'general', label: 'General', icon: Palette },
  { id: 'github', label: 'GitHub Accounts', icon: Github },
  { id: 'gitlab', label: 'GitLab Accounts', icon: Gitlab },
  { id: 'aws', label: 'AWS Accounts', icon: Cloud },
  { id: 'engine', label: 'Engine Paths', icon: Cpu },
  { id: 'ai', label: 'AI & Models', icon: Sparkles },
  { id: 'mcp', label: 'MCP Server', icon: Server },
  { id: 'google', label: 'Google Account', icon: HardDrive },
  { id: 'usage', label: 'Usage & Budget', icon: Gauge },
  { id: 'remote', label: 'Remote Control', icon: Radio },
  { id: 'prompts', label: 'Prompt Templates', icon: FileText },
] as const
const activeSection = ref<(typeof SECTIONS)[number]['id']>('general')
// --- Google accounts (Drive + Gmail via OAuth, multi-account) ---
// The OAuth client (Client ID/Secret) is saved once and reused for every
// account, so adding more accounts only needs a fresh consent — no re-entry.
interface GoogleAccount { id: string; label: string; email: string; scope: string; is_default: boolean; created_at: string }
const googleAccounts = ref<GoogleAccount[]>([])
const googleHasClient = ref(false)
const googleClientId = ref('')
const googleClientSecret = ref('')
const googleNewLabel = ref('')
const googleAdding = ref(false)
const googleError = ref<string | null>(null)
// Reveal the credential form even when creds are already saved (to change them).
const showGoogleClientForm = ref(false)
const googleEditing = ref<string | null>(null)
const googleEditLabel = ref('')

async function loadGoogleStatus() {
  try {
    googleAccounts.value = await invoke<GoogleAccount[]>('list_google_accounts')
    googleHasClient.value = (await invoke<{ has_client: boolean }>('google_client_status')).has_client
  } catch { /* leave defaults */ }
}
async function addGoogleAccount() {
  googleError.value = null
  const label = googleNewLabel.value.trim()
  if (!label) { googleError.value = 'Enter a label for the account (e.g. work, personal).'; return }
  const needCreds = !googleHasClient.value || showGoogleClientForm.value
  if (needCreds && (!googleClientId.value.trim() || !googleClientSecret.value.trim())) {
    googleError.value = 'Enter both Client ID and Client Secret.'
    return
  }
  googleAdding.value = true
  try {
    await invoke<GoogleAccount>('add_google_account', {
      label,
      ...(needCreds ? { clientId: googleClientId.value.trim(), clientSecret: googleClientSecret.value.trim() } : {}),
    })
    googleNewLabel.value = ''
    googleClientSecret.value = ''
    showGoogleClientForm.value = false
    await loadGoogleStatus()
    toast.success('Google account added')
  } catch (e) {
    googleError.value = String(e)
  } finally {
    googleAdding.value = false
  }
}
async function removeGoogleAccount(acc: GoogleAccount) {
  if (!(await confirm({ title: `Remove ${acc.label}?`, message: 'The AI will lose access to this account. Your saved OAuth credentials are kept so you can re-add it without re-entering them.' }))) return
  try {
    await invoke('delete_google_account', { id: acc.id })
    await loadGoogleStatus()
    toast.success('Account removed')
  } catch (e) { googleError.value = String(e) }
}
async function saveGoogleRename(acc: GoogleAccount) {
  const label = googleEditLabel.value.trim()
  if (!label || label === acc.label) { googleEditing.value = null; return }
  try {
    await invoke('rename_google_account', { id: acc.id, label })
    googleEditing.value = null
    await loadGoogleStatus()
  } catch (e) { googleError.value = String(e) }
}
async function makeGoogleDefault(acc: GoogleAccount) {
  try {
    await invoke('set_default_google_account', { id: acc.id })
    await loadGoogleStatus()
  } catch (e) { googleError.value = String(e) }
}
async function forgetGoogleClient() {
  if (!(await confirm({ title: 'Forget saved credentials?', message: 'This removes the stored Client ID/Secret and ALL connected accounts. You will need to paste the credentials again next time.' }))) return
  try {
    await invoke('google_forget_client')
    googleClientId.value = ''
    googleClientSecret.value = ''
    showGoogleClientForm.value = false
    await loadGoogleStatus()
    toast.success('Saved Google credentials removed')
  } catch (e) { googleError.value = String(e) }
}

const ghCount = computed(() => ghStore.accounts.length)
const glCount = computed(() => glStore.accounts.length)
const awsCount = computed(() => awsStore.accounts.length)

// --- GitHub accounts ---
const ghStore = useGithubAccountsStore()
const newLabel = ref('')
const newPat = ref('')
const adding = ref(false)
const addError = ref<string | null>(null)
// Per-account UI state keyed by account id.
const editLabel = ref<Record<string, string>>({})
const editPat = ref<Record<string, string>>({})
const editing = ref<string | null>(null)
const validations = ref<Record<string, PatValidation>>({})
const accountError = ref<Record<string, string>>({})
const busyAccount = ref<string | null>(null)

async function handleAddAccount() {
  if (!newLabel.value.trim() || !newPat.value.trim()) return
  adding.value = true
  addError.value = null
  try {
    await ghStore.create(newLabel.value.trim(), newPat.value.trim())
    newLabel.value = ''
    newPat.value = ''
    toast.success('Account added')
  } catch (e) {
    addError.value = String(e)
  } finally {
    adding.value = false
  }
}

function startEdit(id: string, label: string) {
  editing.value = id
  editLabel.value[id] = label
  editPat.value[id] = ''
}

async function handleSaveEdit(id: string) {
  busyAccount.value = id
  accountError.value[id] = ''
  try {
    await ghStore.update(id, editLabel.value[id]?.trim() || '', editPat.value[id])
    editing.value = null
    toast.success('Account updated')
  } catch (e) {
    accountError.value[id] = String(e)
  } finally {
    busyAccount.value = null
  }
}

async function handleValidate(id: string) {
  busyAccount.value = id
  accountError.value[id] = ''
  delete validations.value[id]
  try {
    validations.value[id] = await ghStore.validate(id)
  } catch (e) {
    accountError.value[id] = String(e)
  } finally {
    busyAccount.value = null
  }
}

async function handleDeleteAccount(id: string) {
  if (!(await confirm({
    title: 'Delete GitHub account',
    message: 'Delete this GitHub account? Projects linked to it will be unlinked.',
    confirmLabel: 'Delete',
  }))) return
  try {
    await ghStore.remove(id)
    toast.success('Account deleted')
  } catch (e) {
    toast.error(String(e))
  }
}

// --- GitLab accounts (mirror of GitHub, plus host + email) ---
const glStore = useGitlabAccountsStore()
const glNewLabel = ref('')
const glNewPat = ref('')
const glNewHost = ref('')
const glNewEmail = ref('')
const glAdding = ref(false)
const glAddError = ref<string | null>(null)
const glEditLabel = ref<Record<string, string>>({})
const glEditPat = ref<Record<string, string>>({})
const glEditHost = ref<Record<string, string>>({})
const glEditEmail = ref<Record<string, string>>({})
const glEditing = ref<string | null>(null)
const glValidations = ref<Record<string, GitlabPatValidation>>({})
const glAccountError = ref<Record<string, string>>({})
const glBusyAccount = ref<string | null>(null)

async function handleAddGitlabAccount() {
  if (!glNewLabel.value.trim() || !glNewPat.value.trim()) return
  glAdding.value = true
  glAddError.value = null
  try {
    await glStore.create(
      glNewLabel.value.trim(),
      glNewPat.value.trim(),
      glNewHost.value.trim(),
      glNewEmail.value.trim(),
    )
    glNewLabel.value = ''
    glNewPat.value = ''
    glNewHost.value = ''
    glNewEmail.value = ''
    toast.success('Account added')
  } catch (e) {
    glAddError.value = String(e)
  } finally {
    glAdding.value = false
  }
}

function startGitlabEdit(id: string, label: string, host: string | null, email: string | null) {
  glEditing.value = id
  glEditLabel.value[id] = label
  glEditPat.value[id] = ''
  glEditHost.value[id] = host ?? ''
  glEditEmail.value[id] = email ?? ''
}

async function handleSaveGitlabEdit(id: string) {
  glBusyAccount.value = id
  glAccountError.value[id] = ''
  try {
    await glStore.update(
      id,
      glEditLabel.value[id]?.trim() || '',
      glEditPat.value[id],
      glEditHost.value[id],
      glEditEmail.value[id],
    )
    glEditing.value = null
    toast.success('Account updated')
  } catch (e) {
    glAccountError.value[id] = String(e)
  } finally {
    glBusyAccount.value = null
  }
}

async function handleValidateGitlab(id: string) {
  glBusyAccount.value = id
  glAccountError.value[id] = ''
  delete glValidations.value[id]
  try {
    glValidations.value[id] = await glStore.validate(id)
  } catch (e) {
    glAccountError.value[id] = String(e)
  } finally {
    glBusyAccount.value = null
  }
}

async function handleDeleteGitlabAccount(id: string) {
  if (!(await confirm({
    title: 'Delete GitLab account',
    message: 'Delete this GitLab account? Projects linked to it will be unlinked.',
    confirmLabel: 'Delete',
  }))) return
  try {
    await glStore.remove(id)
    toast.success('Account deleted')
  } catch (e) {
    toast.error(String(e))
  }
}

// --- AWS accounts (mirror of Git account management, with keys/profile auth) ---
const awsStore = useAwsAccountsStore()
const AWS_AUTH_OPTIONS: { value: AwsAuthMethod; label: string }[] = [
  { value: 'keys', label: 'Access keys' },
  { value: 'profile', label: 'Named profile / SSO' },
]
const awsNewLabel = ref('')
const awsNewAuthMethod = ref<AwsAuthMethod>('keys')
const awsNewRegion = ref('ap-northeast-1')
const awsNewAccessKeyId = ref('')
const awsNewSecretAccessKey = ref('')
const awsNewSessionToken = ref('')
const awsNewProfileName = ref('')
const awsNewTags = ref('')
const awsAdding = ref(false)
const awsAddError = ref<string | null>(null)
const awsEditLabel = ref<Record<string, string>>({})
const awsEditAuthMethod = ref<Record<string, AwsAuthMethod>>({})
const awsEditRegion = ref<Record<string, string>>({})
const awsEditAccessKeyId = ref<Record<string, string>>({})
const awsEditSecretAccessKey = ref<Record<string, string>>({})
const awsEditSessionToken = ref<Record<string, string>>({})
const awsEditProfileName = ref<Record<string, string>>({})
const awsEditTags = ref<Record<string, string>>({})
const awsEditing = ref<string | null>(null)
const awsValidations = ref<Record<string, AwsValidation>>({})
const awsAccountError = ref<Record<string, string>>({})
const awsBusyAccount = ref<string | null>(null)

function maskAccessKey(value: string | null): string {
  if (!value) return ''
  if (value.length <= 8) return value
  return `${value.slice(0, 4)}…${value.slice(-4)}`
}

function awsPayloadFromNew() {
  return {
    label: awsNewLabel.value.trim(),
    authMethod: awsNewAuthMethod.value,
    region: awsNewRegion.value.trim(),
    accessKeyId: awsNewAccessKeyId.value.trim(),
    secretAccessKey: awsNewSecretAccessKey.value.trim(),
    sessionToken: awsNewSessionToken.value.trim(),
    profileName: awsNewProfileName.value.trim(),
    tags: awsNewTags.value.trim(),
  }
}

function awsPayloadForEdit(id: string) {
  const payload: AwsAccountPayload = {
    label: awsEditLabel.value[id]?.trim() || '',
    authMethod: awsEditAuthMethod.value[id],
    region: awsEditRegion.value[id]?.trim() || 'ap-northeast-1',
    accessKeyId: awsEditAccessKeyId.value[id]?.trim(),
    profileName: awsEditProfileName.value[id]?.trim(),
    tags: awsEditTags.value[id]?.trim(),
  }
  const secretAccessKey = awsEditSecretAccessKey.value[id]?.trim() || ''
  const sessionToken = awsEditSessionToken.value[id]?.trim() || ''
  if (secretAccessKey) {
    payload.secretAccessKey = secretAccessKey
    payload.sessionToken = sessionToken
  } else if (sessionToken) {
    payload.sessionToken = sessionToken
  }
  return payload
}

const canAddAwsAccount = computed(() => {
  if (!awsNewLabel.value.trim() || !awsNewRegion.value.trim()) return false
  if (awsNewAuthMethod.value === 'keys') {
    return !!awsNewAccessKeyId.value.trim() && !!awsNewSecretAccessKey.value.trim()
  }
  return !!awsNewProfileName.value.trim()
})

async function handleAddAwsAccount() {
  if (!canAddAwsAccount.value) return
  awsAdding.value = true
  awsAddError.value = null
  try {
    await awsStore.create(awsPayloadFromNew())
    awsNewLabel.value = ''
    awsNewAccessKeyId.value = ''
    awsNewSecretAccessKey.value = ''
    awsNewSessionToken.value = ''
    awsNewProfileName.value = ''
    awsNewTags.value = ''
    toast.success('Account added')
  } catch (e) {
    awsAddError.value = String(e)
  } finally {
    awsAdding.value = false
  }
}

function startAwsEdit(acc: { id: string; label: string; auth_method: AwsAuthMethod; region: string; access_key_id: string | null; profile_name: string | null; tags: string | null }) {
  awsEditing.value = acc.id
  awsEditLabel.value[acc.id] = acc.label
  awsEditAuthMethod.value[acc.id] = acc.auth_method
  awsEditRegion.value[acc.id] = acc.region
  awsEditAccessKeyId.value[acc.id] = acc.access_key_id ?? ''
  awsEditSecretAccessKey.value[acc.id] = ''
  awsEditSessionToken.value[acc.id] = ''
  awsEditProfileName.value[acc.id] = acc.profile_name ?? ''
  awsEditTags.value[acc.id] = acc.tags ?? ''
}

function setAwsEditAuthMethod(id: string, value: string) {
  awsEditAuthMethod.value[id] = value === 'profile' ? 'profile' : 'keys'
}

async function handleSaveAwsEdit(id: string) {
  awsBusyAccount.value = id
  awsAccountError.value[id] = ''
  try {
    await awsStore.update(id, awsPayloadForEdit(id))
    awsEditing.value = null
    toast.success('Account updated')
  } catch (e) {
    awsAccountError.value[id] = String(e)
  } finally {
    awsBusyAccount.value = null
  }
}

async function handleValidateAws(id: string) {
  awsBusyAccount.value = id
  awsAccountError.value[id] = ''
  delete awsValidations.value[id]
  try {
    awsValidations.value[id] = await awsStore.validate(id)
  } catch (e) {
    awsAccountError.value[id] = String(e)
  } finally {
    awsBusyAccount.value = null
  }
}

async function handleDeleteAwsAccount(id: string) {
  if (!(await confirm({
    title: 'Delete AWS account',
    message: 'Delete this AWS account? Projects linked to it will be unlinked.',
    confirmLabel: 'Delete',
  }))) return
  try {
    await awsStore.remove(id)
    toast.success('Account deleted')
  } catch (e) {
    toast.error(String(e))
  }
}

onMounted(async () => {
  try {
    settings.value = await invoke<AppSettings>('get_settings')
    lastSaved = { ...settings.value }
    await ghStore.fetch()
    await glStore.fetch()
    await awsStore.fetch()
    await loadGoogleStatus()
    budget.refresh()
    unlistenPlanUsage = await listen<{ provider?: string }>('plan_usage_updated', (e) => {
      // Claude plan usage feeds the budget guardrail verdict — keep the badge synced.
      if (e.payload?.provider !== 'codex' && !budget.refreshingPlan) budget.refresh()
    })
    unlistenBudgetStatus = await listen('budget_status_updated', () => budget.refresh())
    // Discover the account's Claude models (cached; augments the curated list).
    modelCatalog.fetchClaude().catch(() => {})
  } finally {
    loading.value = false
  }
})

onUnmounted(() => {
  if (unlistenPlanUsage) unlistenPlanUsage()
  if (unlistenBudgetStatus) unlistenBudgetStatus()
})

function applyTheme(theme: string) {
  if (theme === 'dark') {
    document.documentElement.classList.add('dark')
  } else if (theme === 'light') {
    document.documentElement.classList.remove('dark')
  } else {
    const dark = window.matchMedia('(prefers-color-scheme: dark)').matches
    document.documentElement.classList.toggle('dark', dark)
  }
}

function applyColorTheme(theme: string) {
  const t = theme && theme !== 'default' ? theme : ''
  if (t) document.documentElement.setAttribute('data-theme', t)
  else document.documentElement.removeAttribute('data-theme')
}

async function persistChanges() {
  if (!lastSaved) return
  const changed = Object.entries(settings.value).filter(
    ([k, v]) => String(v) !== String(lastSaved![k as keyof AppSettings]),
  )
  if (!changed.length) return
  try {
    for (const [key, value] of changed) {
      await invoke('update_setting', { key, value: String(value) })
    }
    lastSaved = { ...settings.value }
    // Keep the shared settings store (context meter + budget badge) in sync.
    appSettings.refresh().catch(() => {})
    toast.success('Saved')
  } catch (e) {
    toast.error(String(e))
  }
}

// Auto-save: persist (debounced) whenever a setting changes.
watch(settings, () => {
  if (loading.value || !lastSaved) return
  if (saveTimer) clearTimeout(saveTimer)
  saveTimer = setTimeout(persistChanges, 400)
}, { deep: true })

// Apply theme instantly (don't wait for the debounced save).
watch(() => settings.value.theme, (t) => {
  if (!loading.value) applyTheme(t)
})
watch(() => settings.value.color_theme, (t) => {
  if (!loading.value) applyColorTheme(t)
})
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- Header -->
    <div class="flex items-center px-6 h-13 border-b border-border/60 shrink-0">
      <h1 class="text-sm font-semibold">Settings</h1>
    </div>

    <!-- Content -->
    <div class="flex-1 flex min-h-0">
      <!-- Section nav -->
      <nav class="w-48 shrink-0 border-r border-border/60 p-3 overflow-auto">
        <button
          v-for="s in SECTIONS"
          :key="s.id"
          class="w-full flex items-center gap-2.5 px-2.5 py-2 mb-0.5 text-xs rounded-md transition-colors cursor-pointer text-left"
          :class="activeSection === s.id
            ? 'bg-accent text-foreground font-medium'
            : 'text-muted-foreground hover:bg-accent/50 hover:text-foreground'"
          @click="activeSection = s.id"
        >
          <component :is="s.icon" class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
          <span class="truncate">{{ s.label }}</span>
          <span
            v-if="s.id === 'github' && ghCount"
            class="ml-auto text-[10px] tabular-nums text-muted-foreground"
          >{{ ghCount }}</span>
          <span
            v-if="s.id === 'gitlab' && glCount"
            class="ml-auto text-[10px] tabular-nums text-muted-foreground"
          >{{ glCount }}</span>
          <span
            v-if="s.id === 'aws' && awsCount"
            class="ml-auto text-[10px] tabular-nums text-muted-foreground"
          >{{ awsCount }}</span>
        </button>
      </nav>

      <!-- Active section panel -->
      <div class="flex-1 overflow-auto p-6 min-w-0">
        <!-- Loading skeleton -->
        <div v-if="loading" class="max-w-lg space-y-4">
          <div v-for="i in 3" :key="i" class="h-28 bg-card rounded-lg border border-border animate-pulse" />
        </div>

        <div v-else class="max-w-lg">

        <!-- General section -->
        <Card v-show="activeSection === 'general'" body-class="p-4 space-y-4">
          <template #header>
            <Palette class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
            <span class="text-xs font-semibold">General</span>
          </template>
            <div class="space-y-1.5">
              <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Theme</label>
              <AppSelect
                size="sm"
                v-model="settings.theme"
                :options="[
                  { value: 'system', label: 'System' },
                  { value: 'light', label: 'Light' },
                  { value: 'dark', label: 'Dark' },
                ]"
              />
            </div>
            <div class="space-y-1.5">
              <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Color Theme</label>
              <AppSelect
                size="sm"
                v-model="settings.color_theme"
                :options="[
                  { value: 'default', label: 'Default (Indigo)' },
                  { value: 'ocean', label: 'Ocean' },
                  { value: 'forest', label: 'Forest' },
                  { value: 'sunset', label: 'Sunset' },
                  { value: 'rose', label: 'Rose' },
                  { value: 'teal', label: 'Teal' },
                  { value: 'lagoon', label: 'Lagoon' },
                  { value: 'mint', label: 'Mint' },
                  { value: 'midautumn', label: 'Mid-Autumn Night (Đêm Trung Thu) 🎑' },
                ]"
              />
              <p class="text-[11px] text-muted-foreground">Works with both light and dark mode.</p>
            </div>
            <div class="space-y-1.5">
              <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Animated Background</label>
              <AppSelect
                size="sm"
                v-model="settings.animated_background"
                :options="[
                  { value: 'true', label: 'On' },
                  { value: 'false', label: 'Off' },
                ]"
              />
              <p class="text-[11px] text-muted-foreground">Adds a living scene (e.g. the Full Moon 🌕 night sky) for scenic themes. Automatically paused under reduced-motion.</p>
            </div>
            <div class="space-y-1.5">
              <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Terminal App</label>
              <AppSelect
                size="sm"
                v-model="settings.terminal_app"
                :options="[
                  { value: 'terminal', label: 'Terminal (macOS default)' },
                  { value: 'iterm', label: 'iTerm' },
                ]"
              />
            </div>
        </Card>

        <!-- MCP Server section -->
        <Card v-show="activeSection === 'mcp'" body-class="p-4 space-y-4">
          <template #header>
            <Server class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
            <span class="text-xs font-semibold">Built-in MCP Server</span>
          </template>
          <div class="space-y-1.5">
            <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">
              Devdy MCP Server
            </label>
            <AppSelect
              size="sm"
              v-model="settings.mcp_builtin_devdy_enabled"
              :options="[
                { value: 'true', label: 'Enabled (recommended)' },
                { value: 'false', label: 'Disabled' },
              ]"
            />
            <p class="text-[11px] text-muted-foreground">
              Injects a built-in <code>devdy</code> MCP server into every run so the AI can use your
              own data while chatting — quick notes, cross-session recall, project context (file tree
              &amp; git) and managed VPS. Tools appear as <code>mcp__devdy__*</code>; write &amp; remote
              actions still go through the permission prompt. Takes effect on the next run.
            </p>
          </div>
          <div class="rounded-md border border-border/60 bg-muted/30 p-3 space-y-2">
            <div class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Available tools</div>
            <div class="grid grid-cols-2 gap-x-4 gap-y-1 text-[11px] text-muted-foreground">
              <span>📝 notes_list / read / create / update / append</span>
              <span>🧠 sessions_recent / search / read</span>
              <span>✅ todos_list / add / done</span>
              <span>📁 project_info / file_tree</span>
              <span>🔀 git_status / git_diff</span>
              <span>🖥️ vps_list / vps_run</span>
            </div>
          </div>
        </Card>

        <!-- Google Account section (Drive + Gmail) -->
        <Card v-show="activeSection === 'google'" body-class="p-4 space-y-4">
          <template #header>
            <HardDrive class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
            <span class="text-xs font-semibold">Google Account (Drive + Gmail)</span>
          </template>

          <p class="text-[11px] text-muted-foreground leading-relaxed">
            Connect one or more Google accounts so the AI can work with your Drive files and Gmail. Tools
            appear as <code class="font-mono bg-muted px-1 rounded">mcp__gdrive__*</code> and
            <code class="font-mono bg-muted px-1 rounded">mcp__gmail__*</code>; with multiple accounts the
            AI targets one via each tool's <code class="font-mono bg-muted px-1 rounded">account</code>
            argument (the <strong class="text-foreground">default</strong> account when omitted). Write,
            delete, share and send actions still go through the permission prompt. Refresh tokens are
            stored in your OS Keychain (never written to disk). Takes effect on the next run.
          </p>

          <div class="p-2.5 bg-amber-500/10 border border-amber-500/20 rounded-md text-[11px] leading-relaxed flex gap-2">
            <ShieldAlert class="h-3.5 w-3.5 text-amber-500 shrink-0 mt-0.5" :stroke-width="1.75" />
            <span class="text-muted-foreground">
              Create your own OAuth <strong class="text-foreground">Desktop app</strong> credentials in Google
              Cloud Console, enable the <strong class="text-foreground">Drive API</strong> and
              <strong class="text-foreground">Gmail API</strong>, then paste the Client ID and Secret below.
            </span>
          </div>

          <!-- Connected accounts list -->
          <div v-if="googleAccounts.length" class="space-y-2">
            <div
              v-for="acc in googleAccounts"
              :key="acc.id"
              class="border border-border rounded-md p-3 space-y-2"
            >
              <template v-if="googleEditing !== acc.id">
                <div class="flex items-center justify-between gap-2">
                  <div class="min-w-0">
                    <div class="text-sm font-medium truncate flex items-center gap-1.5">
                      <CheckCircle2 class="h-3.5 w-3.5 text-emerald-500" :stroke-width="1.75" />
                      {{ acc.label }}
                      <span v-if="acc.is_default" class="text-[10px] font-medium text-indigo-400 border border-indigo-400/40 rounded px-1">default</span>
                    </div>
                    <div class="text-[11px] text-muted-foreground truncate">{{ acc.email || 'Drive + Gmail' }}</div>
                  </div>
                  <div class="flex items-center gap-1 shrink-0">
                    <Button v-if="!acc.is_default" size="sm" variant="ghost" title="Set as default" @click="makeGoogleDefault(acc)">
                      <CheckCircle2 class="h-3.5 w-3.5" :stroke-width="1.75" />
                    </Button>
                    <Button size="sm" variant="ghost" title="Rename" @click="googleEditing = acc.id; googleEditLabel = acc.label">
                      <Pencil class="h-3.5 w-3.5" :stroke-width="1.75" />
                    </Button>
                    <Button size="sm" variant="ghost" title="Remove" @click="removeGoogleAccount(acc)">
                      <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
                    </Button>
                  </div>
                </div>
              </template>
              <template v-else>
                <div class="flex items-center gap-2">
                  <Input v-model="googleEditLabel" size="sm" class="flex-1" @keyup.enter="saveGoogleRename(acc)" />
                  <Button size="sm" @click="saveGoogleRename(acc)">Save</Button>
                  <Button size="sm" variant="ghost" @click="googleEditing = null">Cancel</Button>
                </div>
              </template>
            </div>
          </div>

          <!-- Add account -->
          <div class="border border-border/60 rounded-md p-3 space-y-2.5">
            <div class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">
              {{ googleAccounts.length ? 'Add another account' : 'Connect an account' }}
            </div>

            <div v-if="googleHasClient && !showGoogleClientForm" class="text-[11px] text-muted-foreground flex items-center gap-1.5">
              <CheckCircle2 class="h-3.5 w-3.5 text-emerald-500" :stroke-width="1.75" />
              OAuth credentials saved — no need to re-enter them.
            </div>

            <!-- Credential form (first time, or when changing creds) -->
            <template v-if="!googleHasClient || showGoogleClientForm">
              <div class="space-y-1.5">
                <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Client ID</label>
                <Input v-model="googleClientId" size="sm" placeholder="xxxxx.apps.googleusercontent.com" />
              </div>
              <div class="space-y-1.5">
                <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Client Secret</label>
                <Input v-model="googleClientSecret" type="password" size="sm" placeholder="GOCSPX-…" />
              </div>
            </template>

            <div class="space-y-1.5">
              <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Account label</label>
              <Input v-model="googleNewLabel" size="sm" placeholder="work" @keyup.enter="addGoogleAccount" />
            </div>

            <div v-if="googleError" class="text-[11px] text-red-500 flex items-start gap-1.5">
              <AlertTriangle class="h-3.5 w-3.5 shrink-0 mt-0.5" :stroke-width="1.75" />
              <span>{{ googleError }}</span>
            </div>

            <div class="flex items-center gap-2 flex-wrap">
              <Button size="sm" :disabled="googleAdding" @click="addGoogleAccount">
                <Loader2 v-if="googleAdding" class="h-3.5 w-3.5 animate-spin" :stroke-width="1.75" />
                <Plus v-else class="h-3.5 w-3.5" :stroke-width="1.75" />
                {{ googleAdding ? 'Waiting for Google…' : 'Add account' }}
              </Button>
              <Button v-if="googleHasClient && !showGoogleClientForm" size="sm" variant="ghost" :disabled="googleAdding" @click="showGoogleClientForm = true">
                <Pencil class="h-3.5 w-3.5" :stroke-width="1.75" /> Change credentials
              </Button>
              <Button v-if="googleHasClient && showGoogleClientForm" size="sm" variant="ghost" :disabled="googleAdding" @click="showGoogleClientForm = false">
                Cancel
              </Button>
              <Button v-if="googleHasClient" size="sm" variant="ghost" :disabled="googleAdding" @click="forgetGoogleClient">
                <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" /> Forget saved
              </Button>
            </div>
          </div>

          <div class="rounded-md border border-border/60 bg-muted/30 p-3 space-y-2">
            <div class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Available tools</div>
            <div class="grid grid-cols-2 gap-x-4 gap-y-1 text-[11px] text-muted-foreground">
              <span>📁 gdrive list / search / read / download</span>
              <span>✏️ gdrive create / upload / update / rename / move</span>
              <span>🗑️ gdrive delete (permanent) · share / permissions</span>
              <span>📧 gmail list / search / read_message / read_thread</span>
              <span>✉️ gmail send / create_draft / reply</span>
              <span>🏷️ gmail modify_labels / mark / trash / delete</span>
            </div>
          </div>
        </Card>

        <!-- GitHub Accounts section -->
        <Card v-show="activeSection === 'github'" body-class="p-4 space-y-4">
          <template #header>
            <Github class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
            <span class="text-xs font-semibold">GitHub Accounts</span>
          </template>
            <p class="text-[11px] text-muted-foreground leading-relaxed">
              Add GitHub accounts once and link them to projects. PATs are stored securely in your OS
              Keychain (never written to disk). Required scopes:
              <code class="font-mono bg-muted px-1 rounded text-[11px]">repo</code> (private) or
              <code class="font-mono bg-muted px-1 rounded text-[11px]">public_repo</code> (public).
            </p>

            <div class="p-2.5 bg-amber-500/10 border border-amber-500/20 rounded-md text-[11px] leading-relaxed flex gap-2">
              <ShieldAlert class="h-3.5 w-3.5 text-amber-500 shrink-0 mt-0.5" :stroke-width="1.75" />
              <span class="text-muted-foreground">
                Keep this machine logged <strong class="text-foreground">out</strong> of gh globally: do
                <strong class="text-foreground">not</strong> run
                <code class="font-mono bg-muted px-1 rounded">gh auth login</code> or set
                <code class="font-mono bg-muted px-1 rounded">GH_TOKEN</code> system-wide. Devdy wires the
                correct per-project credential at run time; a global login would let runs bypass that isolation.
              </span>
            </div>

            <!-- Account list -->
            <div v-if="ghStore.accounts.length" class="space-y-2">
              <div
                v-for="acc in ghStore.accounts"
                :key="acc.id"
                class="border border-border rounded-md p-3 space-y-2"
              >
                <!-- View mode -->
                <template v-if="editing !== acc.id">
                  <div class="flex items-center justify-between gap-2">
                    <div class="min-w-0">
                      <div class="text-sm font-medium truncate">{{ acc.label }}</div>
                      <div class="text-[11px] text-muted-foreground truncate">
                        <span v-if="acc.username">@{{ acc.username }}</span>
                        <span v-else>not validated</span>
                        <span v-if="acc.scopes.length" class="ml-1">· {{ acc.scopes.join(', ') }}</span>
                      </div>
                    </div>
                    <div class="flex items-center gap-1 shrink-0">
                      <Button
                        variant="outline"
                        size="xs"
                        :disabled="busyAccount === acc.id"
                        @click="handleValidate(acc.id)"
                      >
                        {{ busyAccount === acc.id ? '…' : 'Validate' }}
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        title="Edit"
                        @click="startEdit(acc.id, acc.label)"
                      >
                        <Pencil class="h-3.5 w-3.5" :stroke-width="1.75" />
                      </Button>
                      <Button
                        variant="destructive-ghost"
                        size="icon-sm"
                        title="Delete"
                        @click="handleDeleteAccount(acc.id)"
                      >
                        <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
                      </Button>
                    </div>
                  </div>
                  <div
                    v-if="validations[acc.id]"
                    class="p-2 bg-emerald-500/10 border border-emerald-500/20 rounded-md text-[11px]"
                  >
                    <div class="flex items-center gap-1.5 text-emerald-500 font-medium">
                      <CheckCircle2 class="h-3 w-3" :stroke-width="2" />
                      Valid — {{ validations[acc.id].username }}
                    </div>
                    <p v-if="!validations[acc.id].has_repo_scope" class="text-amber-500 mt-1 flex items-center gap-1">
                      <AlertTriangle class="h-3 w-3" :stroke-width="1.75" />
                      Missing repo/public_repo scope
                    </p>
                  </div>
                </template>

                <!-- Edit mode -->
                <template v-else>
                  <Input
                    v-model="editLabel[acc.id]"
                    size="sm"
                    placeholder="Label"
                  />
                  <Input
                    v-model="editPat[acc.id]"
                    type="password"
                    size="sm"
                    placeholder="New PAT (leave blank to keep current)"
                    class="font-mono"
                  />
                  <div class="flex items-center gap-2">
                    <Button
                      :disabled="!editLabel[acc.id]?.trim() || busyAccount === acc.id"
                      @click="handleSaveEdit(acc.id)"
                    >
                      {{ busyAccount === acc.id ? 'Saving…' : 'Save' }}
                    </Button>
                    <Button
                      variant="outline"
                      @click="editing = null"
                    >
                      Cancel
                    </Button>
                  </div>
                </template>

                <p v-if="accountError[acc.id]" class="text-[11px] text-destructive">{{ accountError[acc.id] }}</p>
              </div>
            </div>

            <!-- Add account -->
            <div class="border-t border-border/60 pt-3 space-y-2">
              <div class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Add account</div>
              <Input
                v-model="newLabel"
                size="sm"
                placeholder="Label (e.g. Work, Personal)"
              />
              <div class="flex gap-2">
                <Input
                  v-model="newPat"
                  type="password"
                  size="sm"
                  placeholder="ghp_…"
                  class="flex-1 font-mono"
                  @keyup.enter="handleAddAccount"
                />
                <Button
                  :disabled="!newLabel.trim() || !newPat.trim() || adding"
                  @click="handleAddAccount"
                >
                  <Plus class="h-3.5 w-3.5" :stroke-width="2" />
                  {{ adding ? 'Adding…' : 'Add' }}
                </Button>
              </div>
              <p v-if="addError" class="text-[11px] text-destructive">{{ addError }}</p>
            </div>
        </Card>

        <!-- GitLab Accounts section -->
        <Card v-show="activeSection === 'gitlab'" body-class="p-4 space-y-4">
          <template #header>
            <Gitlab class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
            <span class="text-xs font-semibold">GitLab Accounts</span>
          </template>
            <p class="text-[11px] text-muted-foreground leading-relaxed">
              Add GitLab accounts once and link them to projects. PATs are stored securely in your OS
              Keychain (never written to disk). Set a custom host for self-hosted GitLab, and an optional
              commit email. Required scope:
              <code class="font-mono bg-muted px-1 rounded">api</code> (or
              <code class="font-mono bg-muted px-1 rounded">read_api</code> +
              <code class="font-mono bg-muted px-1 rounded">write_repository</code>) — a repository-only
              token is rejected by the validation endpoint.
            </p>

            <div class="p-2.5 bg-amber-500/10 border border-amber-500/20 rounded-md text-[11px] leading-relaxed flex gap-2">
              <ShieldAlert class="h-3.5 w-3.5 text-amber-500 shrink-0 mt-0.5" :stroke-width="1.75" />
              <span class="text-muted-foreground">
                Keep this machine logged <strong class="text-foreground">out</strong> of glab globally: do
                <strong class="text-foreground">not</strong> run
                <code class="font-mono bg-muted px-1 rounded">glab auth login</code> or set
                <code class="font-mono bg-muted px-1 rounded">GITLAB_TOKEN</code> system-wide. Devdy wires the
                correct per-project credential at run time; a global login would let runs bypass that isolation.
              </span>
            </div>

            <!-- Account list -->
            <div v-if="glStore.accounts.length" class="space-y-2">
              <div
                v-for="acc in glStore.accounts"
                :key="acc.id"
                class="border border-border rounded-md p-3 space-y-2"
              >
                <!-- View mode -->
                <template v-if="glEditing !== acc.id">
                  <div class="flex items-center justify-between gap-2">
                    <div class="min-w-0">
                      <div class="text-sm font-medium truncate">{{ acc.label }}</div>
                      <div class="text-[11px] text-muted-foreground truncate">
                        <span v-if="acc.username">@{{ acc.username }}</span>
                        <span v-else>not validated</span>
                        <span v-if="acc.host" class="ml-1">· {{ acc.host }}</span>
                        <span v-if="acc.email" class="ml-1">· {{ acc.email }}</span>
                      </div>
                    </div>
                    <div class="flex items-center gap-1 shrink-0">
                      <Button
                        variant="outline"
                        size="xs"
                        :disabled="glBusyAccount === acc.id"
                        @click="handleValidateGitlab(acc.id)"
                      >
                        {{ glBusyAccount === acc.id ? '…' : 'Validate' }}
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        title="Edit"
                        @click="startGitlabEdit(acc.id, acc.label, acc.host, acc.email)"
                      >
                        <Pencil class="h-3.5 w-3.5" :stroke-width="1.75" />
                      </Button>
                      <Button
                        variant="destructive-ghost"
                        size="icon-sm"
                        title="Delete"
                        @click="handleDeleteGitlabAccount(acc.id)"
                      >
                        <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
                      </Button>
                    </div>
                  </div>
                  <div
                    v-if="glValidations[acc.id]"
                    class="p-2 bg-emerald-500/10 border border-emerald-500/20 rounded-md text-[11px]"
                  >
                    <div class="flex items-center gap-1.5 text-emerald-500 font-medium">
                      <CheckCircle2 class="h-3 w-3" :stroke-width="2" />
                      Valid — {{ glValidations[acc.id].username }}
                    </div>
                    <p v-if="glValidations[acc.id].email" class="text-muted-foreground mt-1">
                      {{ glValidations[acc.id].email }}
                    </p>
                  </div>
                </template>

                <!-- Edit mode -->
                <template v-else>
                  <Input
                    v-model="glEditLabel[acc.id]"
                    size="sm"
                    placeholder="Label"
                  />
                  <Input
                    v-model="glEditHost[acc.id]"
                    size="sm"
                    placeholder="https://gitlab.com"
                  />
                  <Input
                    v-model="glEditEmail[acc.id]"
                    size="sm"
                    placeholder="Commit email (optional)"
                  />
                  <Input
                    v-model="glEditPat[acc.id]"
                    type="password"
                    size="sm"
                    placeholder="New PAT (leave blank to keep current)"
                    class="font-mono"
                  />
                  <div class="flex items-center gap-2">
                    <Button
                      :disabled="!glEditLabel[acc.id]?.trim() || glBusyAccount === acc.id"
                      @click="handleSaveGitlabEdit(acc.id)"
                    >
                      {{ glBusyAccount === acc.id ? 'Saving…' : 'Save' }}
                    </Button>
                    <Button
                      variant="outline"
                      @click="glEditing = null"
                    >
                      Cancel
                    </Button>
                  </div>
                </template>

                <p v-if="glAccountError[acc.id]" class="text-[11px] text-destructive">{{ glAccountError[acc.id] }}</p>
              </div>
            </div>

            <!-- Add account -->
            <div class="border-t border-border/60 pt-3 space-y-2">
              <div class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Add account</div>
              <Input
                v-model="glNewLabel"
                size="sm"
                placeholder="Label (e.g. Work, Personal)"
              />
              <Input
                v-model="glNewHost"
                size="sm"
                placeholder="https://gitlab.com"
              />
              <Input
                v-model="glNewEmail"
                size="sm"
                placeholder="Commit email (optional)"
              />
              <div class="flex gap-2">
                <Input
                  v-model="glNewPat"
                  type="password"
                  size="sm"
                  placeholder="glpat-…"
                  class="flex-1 font-mono"
                  @keyup.enter="handleAddGitlabAccount"
                />
                <Button
                  :disabled="!glNewLabel.trim() || !glNewPat.trim() || glAdding"
                  @click="handleAddGitlabAccount"
                >
                  <Plus class="h-3.5 w-3.5" :stroke-width="2" />
                  {{ glAdding ? 'Adding…' : 'Add' }}
                </Button>
              </div>
              <p v-if="glAddError" class="text-[11px] text-destructive">{{ glAddError }}</p>
            </div>
        </Card>

        <!-- AWS Accounts section -->
        <Card v-show="activeSection === 'aws'" body-class="p-4 space-y-4">
          <template #header>
            <Cloud class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
            <span class="text-xs font-semibold">AWS Accounts</span>
          </template>
            <p class="text-[11px] text-muted-foreground leading-relaxed">
              Add AWS accounts once and link one account to each project. Secret Access Keys and
              Session Tokens are stored securely in your OS Keychain. Devdy wires the selected
              project account to Claude and Codex runs through the broker.
            </p>

            <div class="p-2.5 bg-amber-500/10 border border-amber-500/20 rounded-md text-[11px] leading-relaxed flex gap-2">
              <ShieldAlert class="h-3.5 w-3.5 text-amber-500 shrink-0 mt-0.5" :stroke-width="1.75" />
              <span class="text-muted-foreground">
                Avoid setting AWS credentials globally for agent work. Devdy gates
                <code class="font-mono bg-muted px-1 rounded">aws</code> calls through its broker;
                credential/config mutation such as
                <code class="font-mono bg-muted px-1 rounded">aws configure</code> is blocked.
              </span>
            </div>

            <!-- Account list -->
            <div v-if="awsStore.accounts.length" class="space-y-2">
              <div
                v-for="acc in awsStore.accounts"
                :key="acc.id"
                class="border border-border rounded-md p-3 space-y-2"
              >
                <template v-if="awsEditing !== acc.id">
                  <div class="flex items-center justify-between gap-2">
                    <div class="min-w-0">
                      <div class="text-sm font-medium truncate">{{ acc.label }}</div>
                      <div class="text-[11px] text-muted-foreground truncate">
                        <span class="uppercase">{{ acc.auth_method }}</span>
                        <span> · {{ acc.region }}</span>
                        <span v-if="acc.account_id"> · {{ acc.account_id }}</span>
                        <span v-if="acc.auth_method === 'keys' && acc.access_key_id"> · {{ maskAccessKey(acc.access_key_id) }}</span>
                        <span v-if="acc.auth_method === 'profile' && acc.profile_name"> · {{ acc.profile_name }}</span>
                      </div>
                    </div>
                    <div class="flex items-center gap-1 shrink-0">
                      <Button
                        variant="outline"
                        size="xs"
                        :disabled="awsBusyAccount === acc.id"
                        @click="handleValidateAws(acc.id)"
                      >
                        {{ awsBusyAccount === acc.id ? '…' : 'Validate' }}
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        title="Edit"
                        @click="startAwsEdit(acc)"
                      >
                        <Pencil class="h-3.5 w-3.5" :stroke-width="1.75" />
                      </Button>
                      <Button
                        variant="destructive-ghost"
                        size="icon-sm"
                        title="Delete"
                        @click="handleDeleteAwsAccount(acc.id)"
                      >
                        <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
                      </Button>
                    </div>
                  </div>
                  <div
                    v-if="awsValidations[acc.id]"
                    class="p-2 bg-emerald-500/10 border border-emerald-500/20 rounded-md text-[11px]"
                  >
                    <div class="flex items-center gap-1.5 text-emerald-500 font-medium">
                      <CheckCircle2 class="h-3 w-3" :stroke-width="2" />
                      Valid — {{ awsValidations[acc.id].account_id }}
                    </div>
                    <p class="text-muted-foreground mt-1 truncate">{{ awsValidations[acc.id].arn }}</p>
                  </div>
                </template>

                <template v-else>
                  <Input v-model="awsEditLabel[acc.id]" size="sm" placeholder="Label" />
                  <AppSelect
                    size="sm"
                    :model-value="awsEditAuthMethod[acc.id]"
                    :options="AWS_AUTH_OPTIONS"
                    @update:model-value="setAwsEditAuthMethod(acc.id, $event)"
                  />
                  <Input v-model="awsEditRegion[acc.id]" size="sm" placeholder="ap-northeast-1" class="font-mono" />
                  <template v-if="awsEditAuthMethod[acc.id] === 'keys'">
                    <Input v-model="awsEditAccessKeyId[acc.id]" size="sm" placeholder="Access Key ID" class="font-mono" />
                    <Input
                      v-model="awsEditSecretAccessKey[acc.id]"
                      type="password"
                      size="sm"
                      placeholder="New Secret Access Key (leave blank to keep current)"
                      class="font-mono"
                    />
                    <Input
                      v-model="awsEditSessionToken[acc.id]"
                      type="password"
                      size="sm"
                      placeholder="Session Token (optional; set when replacing secret)"
                      class="font-mono"
                    />
                  </template>
                  <Input
                    v-else
                    v-model="awsEditProfileName[acc.id]"
                    size="sm"
                    placeholder="AWS profile name"
                    class="font-mono"
                  />
                  <Input v-model="awsEditTags[acc.id]" size="sm" placeholder="Tags (optional)" />
                  <div class="flex items-center gap-2">
                    <Button
                      :disabled="!awsEditLabel[acc.id]?.trim() || awsBusyAccount === acc.id"
                      @click="handleSaveAwsEdit(acc.id)"
                    >
                      {{ awsBusyAccount === acc.id ? 'Saving…' : 'Save' }}
                    </Button>
                    <Button variant="outline" @click="awsEditing = null">Cancel</Button>
                  </div>
                </template>

                <p v-if="awsAccountError[acc.id]" class="text-[11px] text-destructive">{{ awsAccountError[acc.id] }}</p>
              </div>
            </div>

            <!-- Add account -->
            <div class="border-t border-border/60 pt-3 space-y-2">
              <div class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Add account</div>
              <Input v-model="awsNewLabel" size="sm" placeholder="Label (e.g. Work AWS, Production)" />
              <AppSelect
                size="sm"
                v-model="awsNewAuthMethod"
                :options="AWS_AUTH_OPTIONS"
              />
              <Input v-model="awsNewRegion" size="sm" placeholder="ap-northeast-1" class="font-mono" />
              <template v-if="awsNewAuthMethod === 'keys'">
                <Input v-model="awsNewAccessKeyId" size="sm" placeholder="Access Key ID" class="font-mono" />
                <Input
                  v-model="awsNewSecretAccessKey"
                  type="password"
                  size="sm"
                  placeholder="Secret Access Key"
                  class="font-mono"
                />
                <Input
                  v-model="awsNewSessionToken"
                  type="password"
                  size="sm"
                  placeholder="Session Token (optional)"
                  class="font-mono"
                  @keyup.enter="handleAddAwsAccount"
                />
              </template>
              <Input
                v-else
                v-model="awsNewProfileName"
                size="sm"
                placeholder="AWS profile name"
                class="font-mono"
                @keyup.enter="handleAddAwsAccount"
              />
              <Input v-model="awsNewTags" size="sm" placeholder="Tags (optional)" />
              <div class="flex justify-end">
                <Button
                  :disabled="!canAddAwsAccount || awsAdding"
                  @click="handleAddAwsAccount"
                >
                  <Plus class="h-3.5 w-3.5" :stroke-width="2" />
                  {{ awsAdding ? 'Adding…' : 'Add' }}
                </Button>
              </div>
              <p v-if="awsAddError" class="text-[11px] text-destructive">{{ awsAddError }}</p>
            </div>
        </Card>

        <!-- Engine Paths section -->
        <Card v-show="activeSection === 'engine'" body-class="p-4 space-y-4">
          <template #header>
            <Cpu class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
            <span class="text-xs font-semibold">Engine Paths</span>
          </template>
            <div class="space-y-1.5">
              <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Claude binary path</label>
              <Input
                v-model="settings.claude_path"
                size="sm"
                placeholder="/usr/local/bin/claude"
                class="font-mono"
              />
            </div>
            <div class="space-y-1.5">
              <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Codex binary path</label>
              <Input
                v-model="settings.codex_path"
                size="sm"
                placeholder="/usr/local/bin/codex"
                class="font-mono"
              />
            </div>
            <div class="space-y-1.5">
              <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">
                Extra args
                <span class="normal-case font-normal text-muted-foreground ml-1">(applied to all runs)</span>
              </label>
              <Input
                v-model="settings.extra_args"
                size="sm"
                placeholder="--no-cache"
                class="font-mono"
              />
            </div>
        </Card>

        <!-- AI & Models section — run defaults + translation, grouped together -->
        <Card v-show="activeSection === 'ai'" body-class="p-4 space-y-5">
          <template #header>
            <Sparkles class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
            <span class="text-xs font-semibold">AI & Models</span>
          </template>

            <!-- Group 1: run defaults -->
            <div class="space-y-4">
              <div class="flex items-center gap-1.5 text-muted-foreground">
                <Cpu class="h-3 w-3" :stroke-width="1.75" />
                <span class="text-[11px] font-semibold uppercase tracking-wider">Run defaults</span>
              </div>
              <div class="space-y-1.5">
                <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Default Engine</label>
                <AppSelect
                  size="sm"
                  v-model="settings.default_engine"
                  :options="[
                    { value: 'claude', label: 'claude' },
                    { value: 'codex', label: 'codex' },
                  ]"
                />
              </div>
              <div class="space-y-1.5">
                <div class="flex items-center justify-between">
                  <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Default Claude model</label>
                  <button
                    class="inline-flex items-center gap-1 text-[10px] text-muted-foreground hover:text-foreground transition-colors cursor-pointer disabled:opacity-50"
                    :disabled="modelCatalog.loading"
                    title="Tải lại danh sách model từ tài khoản Claude"
                    @click="modelCatalog.fetchClaude(true)"
                  >
                    <component :is="modelCatalog.loading ? Loader2 : RefreshCw" class="h-3 w-3" :class="{ 'animate-spin': modelCatalog.loading }" :stroke-width="1.75" />
                    {{ modelCatalog.loading ? 'Đang tải…' : 'Làm mới' }}
                  </button>
                </div>
                <AppSelect size="sm" v-model="settings.claude_model" :options="claudeModelOptions" />
                <p v-if="modelCatalog.error" class="text-[11px] text-amber-500">Không tải được model động — dùng danh sách mặc định.</p>
                <p v-else-if="modelCatalog.claudeDynamic.length" class="text-[11px] text-muted-foreground">Tự động cập nhật {{ modelCatalog.claudeDynamic.length }} model từ tài khoản.</p>
              </div>
              <div class="space-y-1.5">
                <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Default Codex model</label>
                <AppSelect size="sm" v-model="settings.codex_model" :options="CODEX_MODEL_OPTIONS" />
                <p class="text-[11px] text-muted-foreground">Codex không hỗ trợ liệt kê model động — danh sách này cố định.</p>
              </div>
              <div class="space-y-1.5">
                <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider flex items-center gap-1.5">
                  <ShieldAlert class="h-3 w-3" :stroke-width="1.75" />Default permission mode
                </label>
                <AppSelect
                  size="sm"
                  v-model="settings.default_permission_mode"
                  :options="[
                    { value: 'default', label: 'Ask via UI (default)' },
                    { value: 'acceptEdits', label: 'Auto-accept edits' },
                    { value: 'plan', label: 'Plan only (read-only)' },
                    { value: 'auto', label: 'Auto (classifier)' },
                    { value: 'bypassPermissions', label: 'Bypass all permissions' },
                  ]"
                />
                <p class="text-[11px] text-muted-foreground leading-relaxed">
                  How tool calls are gated (Claude &amp; Codex). "Ask via UI" surfaces each request in a modal;
                  "Bypass all" skips the modal — fast, but unsafe outside trusted directories. You can still
                  override engine, model and permission mode per run on the Run screen.
                </p>
              </div>
            </div>

            <div class="h-px bg-border" />

            <!-- Group 2: translation of highlighted text -->
            <div class="space-y-4">
              <div class="flex items-center gap-1.5 text-muted-foreground">
                <Sparkles class="h-3 w-3" :stroke-width="1.75" />
                <span class="text-[11px] font-semibold uppercase tracking-wider">Translation</span>
              </div>
              <p class="text-[11px] text-muted-foreground leading-relaxed">
                Dùng cho tính năng dịch đoạn văn bản bôi đen trong màn Run AI.
              </p>
              <div class="space-y-1.5">
                <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Translate engine</label>
                <AppSelect
                  size="sm"
                  v-model="settings.translate_engine"
                  :options="[
                    { value: 'claude', label: 'claude' },
                    { value: 'codex', label: 'codex' },
                  ]"
                />
              </div>
              <div class="space-y-1.5">
                <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Translate model</label>
                <AppSelect size="sm" v-model="settings.translate_model" :options="translateModelOptions" />
                <p class="text-[11px] text-muted-foreground">"Default" → chọn model nhanh (Claude dùng Haiku) để dịch nhanh hơn.</p>
              </div>
              <div class="space-y-1.5">
                <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Target language</label>
                <AppSelect size="sm" v-model="settings.translate_target_lang" :options="TRANSLATE_TARGET_OPTIONS" />
              </div>
              <div class="space-y-1.5">
                <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Translation style</label>
                <AppSelect size="sm" v-model="settings.translate_style" :options="TRANSLATE_STYLE_OPTIONS" />
              </div>
            </div>
        </Card>

        <!-- Usage & Budget section -->
        <Card v-show="activeSection === 'usage'" body-class="p-4 space-y-5">
          <template #header>
            <Gauge class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
            <span class="text-xs font-semibold">Usage &amp; Budget</span>
          </template>

            <!-- Context window meter -->
            <div class="space-y-3">
              <div>
                <h3 class="text-xs font-semibold">Context window meter</h3>
                <p class="text-[11px] text-muted-foreground leading-relaxed mt-0.5">
                  Shows how full the current run's context window is. The bar turns amber past the
                  warn threshold and offers a one-click <code class="font-mono bg-muted px-1 rounded">/compact</code>.
                </p>
              </div>
              <div class="grid grid-cols-2 gap-3">
                <div class="space-y-1.5">
                  <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Warn at (%)</label>
                  <Input v-model="settings.context_warn_percent" type="number" min="1" max="100" placeholder="80" />
                </div>
                <div class="space-y-1.5">
                  <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Limit override (tokens)</label>
                  <Input v-model="settings.context_limit_override" type="number" min="0" placeholder="auto from model" />
                </div>
              </div>
              <p class="text-[11px] text-muted-foreground leading-relaxed">
                Leave the override empty to auto-resolve from the model (Claude 200k / 1M, Codex 272k).
              </p>
            </div>

            <hr class="border-border/60" />

            <!-- Usage budget: run-blocking guardrail -->
            <div class="space-y-3">
              <div>
                <h3 class="text-xs font-semibold">Usage budget (blocks new runs)</h3>
                <p class="text-[11px] text-muted-foreground leading-relaxed mt-0.5">
                  Checked before each run &amp; follow-up: Devdy <b>blocks a new run</b> (override per turn) when the
                  run's engine reaches a limit below. Thresholds map to that engine's <b>real subscription plan</b>
                  windows (from <code class="font-mono bg-muted px-1 rounded">/usage</code>) — a rolling <b>5h</b> window
                  and a <b>weekly</b> window (resets per account). A run is blocked if <b>any</b> set window is reached.
                  Codex usually exposes only the weekly window, so set <b>Weekly</b> to cover it. Leave a field empty to
                  skip that window; with no plan data (e.g. API-key sessions) the guardrail is inactive.
                </p>
              </div>
              <div class="grid grid-cols-2 gap-3">
                <div class="space-y-1.5">
                  <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">5h window — block at (%)</label>
                  <Input v-model="settings.budget_5h_percent" type="number" min="1" max="100" placeholder="empty = off" />
                </div>
                <div class="space-y-1.5">
                  <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Weekly window — block at (%)</label>
                  <Input v-model="settings.budget_week_percent" type="number" min="1" max="100" placeholder="empty = off" />
                </div>
              </div>
            </div>
        </Card>

        <!-- Prompt Templates section -->
        <Card v-show="activeSection === 'prompts'" body-class="p-4 space-y-4">
          <template #header>
            <FileText class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
            <span class="text-xs font-semibold">Prompt Templates</span>
          </template>
            <div class="space-y-1.5">
              <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Analyze Issue prompt</label>
              <Textarea
                v-model="settings.analyze_issue_prompt"
                rows="3"
                placeholder="Analyze the following GitHub issue and create a detailed implementation plan…"
              />
            </div>
            <div class="space-y-1.5">
              <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Review PR prompt</label>
              <Textarea
                v-model="settings.review_pr_prompt"
                rows="3"
                placeholder="Review the following pull request and provide detailed feedback…"
              />
            </div>
        </Card>

        </div>

        <!-- Remote Control (wider than the max-w-lg forms to fit the audit table) -->
        <div v-show="!loading && activeSection === 'remote'" class="max-w-3xl">
          <RemoteControlSettings />
        </div>
      </div>
    </div>
  </div>
</template>
