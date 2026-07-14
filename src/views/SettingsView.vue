<script setup lang="ts">
import { ref, onMounted, onUnmounted, computed, watch } from 'vue'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { invoke } from '@/lib/tauri'
import {
  Cpu, Palette, FileText, Check, ShieldAlert, Sparkles, Github, Gitlab,
  CheckCircle2, AlertTriangle, Trash2, Plus, Pencil, Gauge,
} from 'lucide-vue-next'
import { Button, Input, Card, AppSelect } from '@/components/ui'
import { useConfirm } from '@/composables/useConfirm'
import { useGithubAccountsStore, type PatValidation } from '@/stores/githubAccounts'
import { useGitlabAccountsStore, type GitlabPatValidation } from '@/stores/gitlabAccounts'
import { useAppSettingsStore } from '@/stores/appSettings'
import { useBudgetStore } from '@/stores/budget'

const appSettings = useAppSettingsStore()
const budget = useBudgetStore()
const { confirm } = useConfirm()

// Full subscription plan-usage breakdown for the detailed table below. The
// budget store only holds the single guardrail verdict, so Settings fetches the
// raw `/usage` snapshot itself.
interface PlanWindowData { utilization: number | null; resets_at: string | null }
interface PlanUsageData {
  subscription_type: string | null
  rate_limits_available: boolean
  windows: Record<'five_hour' | 'seven_day' | 'seven_day_opus' | 'seven_day_sonnet', PlanWindowData>
}
const planUsage = ref<PlanUsageData | null>(null)
const now = ref(Date.now())
let unlistenPlanUsage: UnlistenFn | null = null
let unlistenBudgetStatus: UnlistenFn | null = null
let clockTimer: ReturnType<typeof setInterval> | null = null

async function refreshPlanUsage() {
  planUsage.value = await invoke<PlanUsageData | null>('get_plan_usage')
}

// Read-only view of the real subscription plan usage, refreshed on mount.
const PLAN_WINDOWS: { key: 'five_hour' | 'seven_day' | 'seven_day_opus' | 'seven_day_sonnet'; label: string }[] = [
  { key: 'five_hour', label: 'Current session (5h)' },
  { key: 'seven_day', label: 'This week (all models)' },
  { key: 'seven_day_opus', label: 'This week (Opus)' },
  { key: 'seven_day_sonnet', label: 'This week (Sonnet)' },
]
const planRows = computed(() => {
  const u = planUsage.value
  if (!u || !u.rate_limits_available) return []
  return PLAN_WINDOWS.map((w) => {
    const win = u.windows[w.key]
    return {
      label: w.label,
      utilization: win?.utilization ?? null,
      resets_at: win?.resets_at ?? null,
    }
  }).filter((r) => r.utilization != null)
})
function planResetText(iso: string | null): string {
  if (!iso) return ''
  const ms = new Date(iso).getTime() - now.value
  if (ms <= 0) return 'resets soon'
  const totalMinutes = Math.max(1, Math.ceil(ms / 60_000))
  const days = Math.floor(totalMinutes / 1440)
  const hours = Math.floor((totalMinutes % 1440) / 60)
  const minutes = totalMinutes % 60
  if (days > 0) return hours > 0 ? `resets in ${days}d ${hours}h` : `resets in ${days}d`
  if (hours > 0) return minutes > 0 ? `resets in ${hours}h ${minutes}m` : `resets in ${hours}h`
  return `resets in ${minutes}m`
}

interface AppSettings {
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

const settings = ref<AppSettings>({
  default_engine: 'claude',
  claude_path: 'claude',
  codex_path: 'codex',
  claude_model: '',
  codex_model: '',
  extra_args: '',
  theme: 'system',
  color_theme: 'default',
  analyze_issue_prompt: '',
  review_pr_prompt: '',
  default_permission_mode: 'default',
  terminal_app: 'terminal',
  context_warn_percent: '80',
  context_limit_override: '',
  token_budget_period: 'month',
  token_budget_limit: '',
  budget_warn_percent: '80',
})

// `[1m]` selects the 1M-context variant; the bare alias uses the 200K default.
// Aliases (not pinned ids) keep these current as new model versions ship.
const CLAUDE_MODEL_OPTIONS = [
  { value: '', label: 'Default (engine decides)' },
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
const saved = ref(false)
// Snapshot of the last persisted settings so the auto-save watcher only
// pushes keys that actually changed.
let lastSaved: AppSettings | null = null
let saveTimer: ReturnType<typeof setTimeout> | null = null
let savedTimer: ReturnType<typeof setTimeout> | null = null

const SECTIONS = [
  { id: 'general', label: 'General', icon: Palette },
  { id: 'github', label: 'GitHub Accounts', icon: Github },
  { id: 'gitlab', label: 'GitLab Accounts', icon: Gitlab },
  { id: 'engine', label: 'Engine Paths', icon: Cpu },
  { id: 'models', label: 'Default Models', icon: Sparkles },
  { id: 'permissions', label: 'Permissions', icon: ShieldAlert },
  { id: 'usage', label: 'Usage & Budget', icon: Gauge },
  { id: 'prompts', label: 'Prompt Templates', icon: FileText },
] as const
const activeSection = ref<(typeof SECTIONS)[number]['id']>('general')
const ghCount = computed(() => ghStore.accounts.length)
const glCount = computed(() => glStore.accounts.length)

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
  } catch (e) {
    alert(String(e))
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
  } catch (e) {
    alert(String(e))
  }
}

onMounted(async () => {
  try {
    settings.value = await invoke<AppSettings>('get_settings')
    lastSaved = { ...settings.value }
    await ghStore.fetch()
    await glStore.fetch()
    now.value = Date.now()
    clockTimer = setInterval(() => { now.value = Date.now() }, 30_000)
    budget.refresh()
    await refreshPlanUsage()
    unlistenPlanUsage = await listen('plan_usage_updated', () => {
      refreshPlanUsage().catch(() => {})
      if (!budget.refreshingPlan) budget.refresh()
    })
    unlistenBudgetStatus = await listen('budget_status_updated', () => budget.refresh())
  } finally {
    loading.value = false
  }
})

onUnmounted(() => {
  if (clockTimer) clearInterval(clockTimer)
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
    saved.value = true
    if (savedTimer) clearTimeout(savedTimer)
    savedTimer = setTimeout(() => { saved.value = false }, 1500)
  } catch (e) {
    alert(String(e))
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
    <div class="flex items-center justify-between px-6 h-13 border-b border-border/60 shrink-0">
      <h1 class="text-sm font-semibold">Settings</h1>
      <Transition
        enter-active-class="transition-opacity duration-150"
        enter-from-class="opacity-0"
        leave-active-class="transition-opacity duration-300"
        leave-to-class="opacity-0"
      >
        <span
          v-if="saved"
          class="flex items-center gap-1 text-xs text-emerald-500"
        >
          <Check class="h-3.5 w-3.5" :stroke-width="2.5" />
          Saved
        </span>
      </Transition>
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
              <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Default Engine</label>
              <AppSelect
                v-model="settings.default_engine"
                :options="[
                  { value: 'claude', label: 'claude' },
                  { value: 'codex', label: 'codex' },
                ]"
              />
            </div>
            <div class="space-y-1.5">
              <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Theme</label>
              <AppSelect
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
                v-model="settings.color_theme"
                :options="[
                  { value: 'default', label: 'Default (Indigo)' },
                  { value: 'ocean', label: 'Ocean' },
                  { value: 'forest', label: 'Forest' },
                  { value: 'sunset', label: 'Sunset' },
                  { value: 'rose', label: 'Rose' },
                ]"
              />
              <p class="text-[11px] text-muted-foreground">Works with both light and dark mode.</p>
            </div>
            <div class="space-y-1.5">
              <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Terminal App</label>
              <AppSelect
                v-model="settings.terminal_app"
                :options="[
                  { value: 'terminal', label: 'Terminal (macOS default)' },
                  { value: 'iterm', label: 'iTerm' },
                ]"
              />
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
                    size="md"
                    placeholder="Label"
                  />
                  <Input
                    v-model="editPat[acc.id]"
                    type="password"
                    size="md"
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
                size="md"
                placeholder="Label (e.g. Work, Personal)"
              />
              <div class="flex gap-2">
                <Input
                  v-model="newPat"
                  type="password"
                  size="md"
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
                    size="md"
                    placeholder="Label"
                  />
                  <Input
                    v-model="glEditHost[acc.id]"
                    size="md"
                    placeholder="https://gitlab.com"
                  />
                  <Input
                    v-model="glEditEmail[acc.id]"
                    size="md"
                    placeholder="Commit email (optional)"
                  />
                  <Input
                    v-model="glEditPat[acc.id]"
                    type="password"
                    size="md"
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
                size="md"
                placeholder="Label (e.g. Work, Personal)"
              />
              <Input
                v-model="glNewHost"
                size="md"
                placeholder="https://gitlab.com"
              />
              <Input
                v-model="glNewEmail"
                size="md"
                placeholder="Commit email (optional)"
              />
              <div class="flex gap-2">
                <Input
                  v-model="glNewPat"
                  type="password"
                  size="md"
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
                size="md"
                placeholder="/usr/local/bin/claude"
                class="font-mono"
              />
            </div>
            <div class="space-y-1.5">
              <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Codex binary path</label>
              <Input
                v-model="settings.codex_path"
                size="md"
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
                size="md"
                placeholder="--no-cache"
                class="font-mono"
              />
            </div>
        </Card>

        <!-- Default Models section -->
        <Card v-show="activeSection === 'models'" body-class="p-4 space-y-4">
          <template #header>
            <Sparkles class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
            <span class="text-xs font-semibold">Default Models</span>
          </template>
            <div class="space-y-1.5">
              <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Claude model</label>
              <AppSelect v-model="settings.claude_model" :options="CLAUDE_MODEL_OPTIONS" />
            </div>
            <div class="space-y-1.5">
              <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Codex model</label>
              <AppSelect v-model="settings.codex_model" :options="CODEX_MODEL_OPTIONS" />
            </div>
            <p class="text-[11px] text-muted-foreground leading-relaxed">
              The default model used when a run doesn't pick one. You can still override the model per run
              on the Run screen. "Default" lets the engine/subscription choose.
            </p>
        </Card>

        <!-- Permissions section -->
        <Card v-show="activeSection === 'permissions'" body-class="p-4 space-y-4">
          <template #header>
            <ShieldAlert class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
            <span class="text-xs font-semibold">Permissions</span>
          </template>
            <div class="space-y-1.5">
              <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">
                Default permission mode
              </label>
              <AppSelect
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
                Choose how tool calls are gated. Applies to both Claude and Codex runs. "Ask via UI" surfaces
                each request in a modal so you can approve or deny it. "Bypass all" skips the modal entirely —
                fast, but unsafe outside trusted directories.
              </p>
              <p class="text-[11px] text-muted-foreground leading-relaxed">
                For Codex this maps to its approval policy &amp; sandbox: <b>Plan</b> → read-only,
                <b>Auto-accept edits</b> → workspace-write (approve on failure), <b>Bypass all</b> →
                full access, <b>others</b> → workspace-write (approve on request).
              </p>
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

            <!-- Real subscription plan usage (from /usage) -->
            <div class="space-y-3">
              <div>
                <h3 class="text-xs font-semibold">Plan usage (Claude subscription)</h3>
                <p class="text-[11px] text-muted-foreground leading-relaxed mt-0.5">
                  Your account's <b>real</b> usage and reset times, read live from Claude's
                  <code class="font-mono bg-muted px-1 rounded">/usage</code> data during runs.
                  Updates after each Claude run on this machine.
                </p>
              </div>
              <div v-if="planRows.length" class="space-y-2">
                <div
                  v-for="row in planRows"
                  :key="row.label"
                  class="space-y-1"
                >
                  <div class="flex items-baseline justify-between text-[11px]">
                    <span class="font-medium">{{ row.label }}</span>
                    <span class="font-mono text-muted-foreground">
                      {{ Math.round(row.utilization!) }}%
                      <span v-if="planResetText(row.resets_at)"> · {{ planResetText(row.resets_at) }}</span>
                    </span>
                  </div>
                  <div class="h-1.5 w-full overflow-hidden rounded-full bg-muted">
                    <div
                      class="h-full rounded-full transition-all"
                      :class="row.utilization! >= 100 ? 'bg-red-500' : row.utilization! >= 80 ? 'bg-amber-500' : 'bg-indigo-500'"
                      :style="{ width: Math.min(100, Math.round(row.utilization!)) + '%' }"
                    />
                  </div>
                </div>
                <p v-if="planUsage?.subscription_type" class="text-[11px] text-muted-foreground">
                  Plan: <b class="capitalize">{{ planUsage.subscription_type }}</b>
                </p>
              </div>
              <p v-else class="text-[11px] text-muted-foreground italic">
                No plan usage captured yet — run a Claude task and it will populate here.
                (Unavailable for API-key / non-subscription sessions.)
              </p>
            </div>

            <hr class="border-border/60" />

            <!-- Global token budget -->
            <div class="space-y-3">
              <div>
                <h3 class="text-xs font-semibold">Usage budget (all runs)</h3>
                <p class="text-[11px] text-muted-foreground leading-relaxed mt-0.5">
                  A guardrail across every run. Devdy warns near the limit and <b>blocks new runs &amp; follow-ups</b>
                  once exceeded (override per turn). For period <b>5h</b>/<b>week</b> it tracks your <b>real Claude
                  subscription plan</b> (from <code class="font-mono bg-muted px-1 rounded">/usage</code>); the token
                  field below is a <b>fallback</b> used only when no plan data applies — Codex, monthly period, or
                  API-key sessions. Counts only runs executed by Devdy; windows are computed in UTC.
                </p>
              </div>
              <div class="grid grid-cols-2 gap-3">
                <div class="space-y-1.5">
                  <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Period</label>
                  <AppSelect
                    v-model="settings.token_budget_period"
                    :options="[
                      { value: 'month', label: 'Monthly (resets 1st)' },
                      { value: 'week', label: 'Weekly (resets Monday)' },
                      { value: '5h', label: 'Rolling 5h (matches Claude)' },
                    ]"
                  />
                </div>
                <div class="space-y-1.5" :class="{ 'opacity-50': budget.hasPlan }">
                  <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Fallback budget (tokens)</label>
                  <Input v-model="settings.token_budget_limit" type="number" min="0" placeholder="empty = disabled" />
                </div>
              </div>
              <p v-if="budget.hasPlan" class="text-[11px] text-indigo-500">
                Currently enforcing the real subscription plan limit ({{ budget.percent }}% used) — the fallback token value is ignored for this period.
              </p>
              <div class="space-y-1.5">
                <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Warn at (%)</label>
                <Input v-model="settings.budget_warn_percent" type="number" min="1" max="100" placeholder="80" />
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
              <textarea
                v-model="settings.analyze_issue_prompt"
                rows="3"
                placeholder="Analyze the following GitHub issue and create a detailed implementation plan…"
                class="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-1 focus:ring-ring transition-colors resize-y"
              />
            </div>
            <div class="space-y-1.5">
              <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Review PR prompt</label>
              <textarea
                v-model="settings.review_pr_prompt"
                rows="3"
                placeholder="Review the following pull request and provide detailed feedback…"
                class="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-1 focus:ring-ring transition-colors resize-y"
              />
            </div>
        </Card>

        </div>
      </div>
    </div>
  </div>
</template>
