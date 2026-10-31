<script setup lang="ts">
// The Notes window's "Secret" tab: notes sealed in the vault (lib/secretVault, backend
// src-tauri/src/secret_vault.rs). They are not in the notes table, so no MCP tool, agent or
// remote controller reads them, and they are never offered to a session.
//
// States: no vault → setup (then the recovery key, shown once); locked → unlock with the
// password or the recovery key (which then asks for a new password); unlocked → list + detail.
// A note's content is fetched only on Reveal and hidden again after REVEAL_SECS or when the
// window loses focus; Copy happens in the backend (cleared from the clipboard after 30 s).
// Content never goes to localStorage, the URL, toasts or logs. Typing in the editor pushes the
// backend's idle auto-lock back (vaultTouch); when the vault locks anyway, open drafts are dropped.
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { getCurrentWindow } from '@tauri-apps/api/window'
import type { UnlistenFn } from '@tauri-apps/api/event'
import {
  Copy, Eye, EyeOff, Folder, KeyRound, Loader2, Lock, LockKeyhole, Pencil, Plus, Search, ShieldCheck, Trash2, X,
} from 'lucide-vue-next'
import { AppSelect, Button, Input, Textarea } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { useConfirm } from '@/composables/useConfirm'
import { useProjectsStore } from '@/stores/projects'
import { relativeAge } from '@/lib/itemText'
import {
  MAX_SECRET_CONTENT,
  REVEAL_SECS,
  contentBytes,
  filterSecretNotes,
  onVaultChanged,
  passwordProblem,
  secretNoteCopy,
  secretNoteCreate,
  secretNoteDelete,
  secretNoteReveal,
  secretNoteSetProject,
  secretNoteUpdate,
  secretNotesList,
  vaultChangePassword,
  vaultError,
  vaultLock,
  vaultMergeOld,
  vaultSetup,
  vaultStatus,
  vaultTouch,
  vaultUnlock,
  vaultUnlockRecovery,
  type SecretNoteMeta,
  type VaultStatus,
} from '@/lib/secretVault'

const props = defineProps<{
  /** The project the window was opened from: default filter and project of a new note. */
  contextProjectId: string | null
}>()

const { t } = useI18n()
const { toast } = useToast()
const { confirm } = useConfirm()
const projects = useProjectsStore()

const NO_PROJECT = '__none__'

function errText(e: unknown): string {
  const { code, wait } = vaultError(e)
  return code ? t(`secretNotes.errors.${code}`, { n: wait ?? 0 }) : String(e)
}

const status = ref<VaultStatus | null>(null)
const notes = ref<SecretNoteMeta[]>([])
const loadError = ref('')

async function refresh() {
  try {
    status.value = await vaultStatus()
    notes.value = status.value.unlocked ? await secretNotesList() : []
    loadError.value = ''
  } catch (e) {
    loadError.value = errText(e)
  }
  if (!status.value?.unlocked) forgetOpen()
  else if (selectedId.value && !notes.value.some((n) => n.id === selectedId.value)) select(null)
}

/** Drop everything sensitive the pane holds (on lock). */
function forgetOpen() {
  hideContent()
  editing.value = false
  draftTitle.value = ''
  draftContent.value = ''
  selectedId.value = null
  // Not the recovery key: shown only while unlocked, it must survive a lock until acknowledged.
  cancelPasswordChange()
  mustChangePassword.value = false
  mergePw.value = ''
}

// ── Setup ──────────────────────────────────────────────────────────────────
const setupPw = ref('')
const setupPw2 = ref('')
const settingUp = ref(false)
/** Shown once after setup, until acknowledged. */
const recoveryKey = ref<string | null>(null)
const setupProblem = computed(() => (setupPw.value ? passwordProblem(setupPw.value, setupPw2.value) : null))

async function setup() {
  if (settingUp.value || passwordProblem(setupPw.value, setupPw2.value)) return
  settingUp.value = true
  try {
    recoveryKey.value = await vaultSetup(setupPw.value)
    setupPw.value = ''
    setupPw2.value = ''
    await refresh()
  } catch (e) {
    toast.error(errText(e))
  } finally {
    settingUp.value = false
  }
}

// ── Unlock ─────────────────────────────────────────────────────────────────
const unlockPw = ref('')
const useRecovery = ref(false)
const recoveryInput = ref('')
const unlocking = ref(false)
const unlockError = ref('')
/** Unlocked with the recovery key: ask for a new password. */
const mustChangePassword = ref(false)

async function unlock() {
  if (unlocking.value) return
  unlocking.value = true
  unlockError.value = ''
  try {
    if (useRecovery.value) {
      await vaultUnlockRecovery(recoveryInput.value)
      mustChangePassword.value = true
      changingPassword.value = true
    } else {
      await vaultUnlock(unlockPw.value)
    }
    unlockPw.value = ''
    recoveryInput.value = ''
    useRecovery.value = false
    await refresh()
  } catch (e) {
    unlockError.value = errText(e)
  } finally {
    unlocking.value = false
  }
}

async function lockNow() {
  await vaultLock().catch(() => {})
  await refresh()
}

// ── Change password ──────────────────────────────────────────────────────────
const changingPassword = ref(false)
const currentPw = ref('')
const newPw = ref('')
const newPw2 = ref('')
const savingPassword = ref(false)
const newPwProblem = computed(() => (newPw.value ? passwordProblem(newPw.value, newPw2.value) : null))

async function changePassword() {
  if (savingPassword.value || passwordProblem(newPw.value, newPw2.value)) return
  if (!mustChangePassword.value && !currentPw.value) return
  savingPassword.value = true
  try {
    const key = await vaultChangePassword(mustChangePassword.value ? null : currentPw.value, newPw.value)
    cancelPasswordChange()
    mustChangePassword.value = false
    toast.success(t('secretNotes.password.changed'))
    // A new vault: the old recovery key opens nothing any more.
    recoveryKey.value = key
  } catch (e) {
    toast.error(errText(e))
    await refresh()
  } finally {
    savingPassword.value = false
  }
}

function cancelPasswordChange() {
  currentPw.value = ''
  newPw.value = ''
  newPw2.value = ''
  changingPassword.value = false
}

// ── Merge older vaults ─────────────────────────────────────────────────────────
const mergePw = ref('')
const merging = ref(false)

async function merge() {
  if (merging.value || !mergePw.value) return
  merging.value = true
  try {
    const n = await vaultMergeOld(mergePw.value)
    mergePw.value = ''
    toast.success(t('secretNotes.merge.done', { n }))
    await refresh()
  } catch (e) {
    toast.error(errText(e))
  } finally {
    merging.value = false
  }
}

// ── List ─────────────────────────────────────────────────────────────────────
const search = ref('')
const projectFilter = ref<string>(props.contextProjectId ?? '')
const visible = computed(() => filterSecretNotes(notes.value, search.value, projectFilter.value, NO_PROJECT))

const projectNames = computed<Record<string, string>>(() =>
  Object.fromEntries(projects.projects.map((p) => [p.id, p.name])),
)
const filterOptions = computed(() => [
  { value: '', label: t('item.allProjects') },
  ...projects.projects.map((p) => ({ value: p.id, label: p.name })).sort((a, b) => a.label.localeCompare(b.label)),
  { value: NO_PROJECT, label: t('item.noProject') },
])
const projectOptions = computed(() => [
  { value: '', label: t('item.noProject') },
  ...projects.projects.map((p) => ({ value: p.id, label: p.name })).sort((a, b) => a.label.localeCompare(b.label)),
])

const selectedId = ref<string | null>(null)
const selected = computed(() => notes.value.find((n) => n.id === selectedId.value) ?? null)

function select(id: string | null) {
  if (editing.value && id !== selectedId.value) cancelEdit()
  hideContent()
  selectedId.value = id
}

// ── Reveal / copy ─────────────────────────────────────────────────────────────
const content = ref<string | null>(null)
const revealing = ref(false)
let hideTimer: ReturnType<typeof setTimeout> | null = null

function hideContent() {
  content.value = null
  if (hideTimer) clearTimeout(hideTimer)
  hideTimer = null
}

async function reveal() {
  const id = selectedId.value
  if (!id || revealing.value) return
  revealing.value = true
  try {
    const text = await secretNoteReveal(id)
    if (selectedId.value !== id) return
    content.value = text
    if (hideTimer) clearTimeout(hideTimer)
    hideTimer = setTimeout(hideContent, REVEAL_SECS * 1000)
  } catch (e) {
    toast.error(errText(e))
    await refresh()
  } finally {
    revealing.value = false
  }
}

async function copy() {
  if (!selectedId.value) return
  try {
    const secs = await secretNoteCopy(selectedId.value)
    toast.success(t('secretNotes.copied', { n: secs }))
  } catch (e) {
    toast.error(errText(e))
    await refresh()
  }
}

// ── Edit / create ───────────────────────────────────────────────────────────────
const editing = ref(false)
/** Editing a new note (not yet in the vault). */
const creatingNew = ref(false)
const draftTitle = ref('')
const draftContent = ref('')
const saving = ref(false)
const tooLarge = computed(() => contentBytes(draftContent.value) > MAX_SECRET_CONTENT)

function startCreate() {
  hideContent()
  selectedId.value = null
  creatingNew.value = true
  draftTitle.value = ''
  draftContent.value = ''
  editing.value = true
}

async function startEdit() {
  const id = selectedId.value
  if (!id) return
  try {
    const text = await secretNoteReveal(id)
    hideContent()
    draftTitle.value = selected.value?.title ?? ''
    draftContent.value = text
    creatingNew.value = false
    editing.value = true
  } catch (e) {
    toast.error(errText(e))
    await refresh()
  }
}

function cancelEdit() {
  editing.value = false
  creatingNew.value = false
  draftTitle.value = ''
  draftContent.value = ''
}

async function save() {
  if (saving.value || tooLarge.value || (!draftTitle.value.trim() && !draftContent.value.trim())) return
  saving.value = true
  try {
    if (creatingNew.value) {
      const project = projectFilter.value && projectFilter.value !== NO_PROJECT ? projectFilter.value : null
      const m = await secretNoteCreate(draftTitle.value.trim(), draftContent.value, project)
      selectedId.value = m.id
    } else if (selectedId.value) {
      await secretNoteUpdate(selectedId.value, draftTitle.value.trim(), draftContent.value)
    }
    cancelEdit()
    await refresh()
  } catch (e) {
    toast.error(errText(e))
    if (vaultError(e).code === 'vault_locked') await refresh()
  } finally {
    saving.value = false
  }
}

// Typing keeps the vault open: the backend locks after its idle time without calls.
let lastTouch = 0
watch([draftTitle, draftContent], () => {
  if (!editing.value || Date.now() - lastTouch < 30_000) return
  lastTouch = Date.now()
  vaultTouch().catch(() => void refresh())
})

async function setProject(projectId: string) {
  if (!selectedId.value) return
  try {
    await secretNoteSetProject(selectedId.value, projectId || null)
    await refresh()
  } catch (e) {
    toast.error(errText(e))
  }
}

async function remove() {
  const m = selected.value
  if (!m) return
  const ok = await confirm({
    title: t('secretNotes.delete.title'),
    message: t('secretNotes.delete.message', { title: m.title || t('secretNotes.untitled') }),
    confirmLabel: t('common.delete'),
    variant: 'destructive',
  })
  if (!ok) return
  try {
    await secretNoteDelete(m.id)
    select(null)
    await refresh()
  } catch (e) {
    toast.error(errText(e))
  }
}

// ── Lifecycle ────────────────────────────────────────────────────────────────────
const unlisten: UnlistenFn[] = []

onMounted(async () => {
  unlisten.push(
    await onVaultChanged(() => void refresh()),
    // Revealed content hides as soon as the window loses focus.
    await getCurrentWindow().onFocusChanged(({ payload: focused }) => {
      if (!focused) hideContent()
    }),
  )
  if (!projects.projects.length) void projects.fetchProjects()
  await refresh()
})

onBeforeUnmount(() => {
  unlisten.forEach((off) => off())
  forgetOpen()
})
</script>

<template>
  <!-- Loading / error -->
  <div v-if="!status" class="flex min-w-0 flex-1 items-center justify-center p-6 text-xs text-muted-foreground">
    <span v-if="loadError" class="text-destructive">{{ loadError }}</span>
    <Loader2 v-else class="h-4 w-4 animate-spin" :stroke-width="1.75" />
  </div>

  <!-- Recovery key, once, right after setup -->
  <div v-else-if="recoveryKey && status.unlocked" class="flex min-w-0 flex-1 items-center justify-center p-6">
    <div class="w-full max-w-md space-y-3">
      <div class="flex items-center gap-2">
        <KeyRound class="h-4 w-4 text-primary" :stroke-width="1.75" />
        <h2 class="text-sm font-semibold">{{ t('secretNotes.recovery.title') }}</h2>
      </div>
      <p class="text-xs text-muted-foreground">{{ t('secretNotes.recovery.help') }}</p>
      <p class="select-all break-all rounded-md border border-border/60 bg-muted/40 p-3 font-mono text-sm tracking-wide">
        {{ recoveryKey }}
      </p>
      <p class="text-[11px] text-amber-600 dark:text-amber-400">{{ t('secretNotes.recovery.warning') }}</p>
      <Button size="sm" @click="recoveryKey = null">{{ t('secretNotes.recovery.done') }}</Button>
    </div>
  </div>

  <!-- No vault yet -->
  <div v-else-if="!status.exists" class="flex min-w-0 flex-1 items-center justify-center p-6">
    <form class="w-full max-w-sm space-y-3" @submit.prevent="setup">
      <div class="flex items-center gap-2">
        <ShieldCheck class="h-4 w-4 text-primary" :stroke-width="1.75" />
        <h2 class="text-sm font-semibold">{{ t('secretNotes.setup.title') }}</h2>
      </div>
      <p class="text-xs text-muted-foreground">{{ t('secretNotes.setup.help') }}</p>
      <p v-if="status.pending_count" class="text-[11px] text-amber-600 dark:text-amber-400">
        {{ t('secretNotes.pending', { n: status.pending_count }) }}
      </p>
      <Input v-model="setupPw" type="password" autocomplete="new-password" :placeholder="t('secretNotes.setup.password')" />
      <Input v-model="setupPw2" type="password" autocomplete="new-password" :placeholder="t('secretNotes.setup.confirm')" />
      <p v-if="setupProblem" class="text-[11px] text-destructive">{{ t(`secretNotes.password.${setupProblem}`) }}</p>
      <Button type="submit" size="sm" :disabled="settingUp || !!passwordProblem(setupPw, setupPw2)">
        <component :is="settingUp ? Loader2 : ShieldCheck" class="h-3.5 w-3.5" :class="{ 'animate-spin': settingUp }" :stroke-width="1.75" />
        {{ t('secretNotes.setup.button') }}
      </Button>
    </form>
  </div>

  <!-- Locked -->
  <div v-else-if="!status.unlocked" class="flex min-w-0 flex-1 items-center justify-center p-6">
    <form class="w-full max-w-sm space-y-3" @submit.prevent="unlock">
      <div class="flex items-center gap-2">
        <LockKeyhole class="h-4 w-4 text-primary" :stroke-width="1.75" />
        <h2 class="text-sm font-semibold">{{ t('secretNotes.unlock.title') }}</h2>
      </div>
      <p class="text-xs text-muted-foreground">{{ t('secretNotes.unlock.help', { n: status.note_count }) }}</p>
      <Input
        v-if="!useRecovery"
        v-model="unlockPw"
        type="password"
        autocomplete="current-password"
        :placeholder="t('secretNotes.unlock.password')"
      />
      <Input
        v-else
        v-model="recoveryInput"
        autocomplete="off"
        spellcheck="false"
        class="font-mono"
        :placeholder="t('secretNotes.unlock.recovery')"
      />
      <p v-if="unlockError" class="text-[11px] text-destructive">{{ unlockError }}</p>
      <div class="flex items-center gap-2">
        <Button type="submit" size="sm" :disabled="unlocking || (useRecovery ? !recoveryInput.trim() : !unlockPw)">
          <component :is="unlocking ? Loader2 : Lock" class="h-3.5 w-3.5" :class="{ 'animate-spin': unlocking }" :stroke-width="1.75" />
          {{ t('secretNotes.unlock.button') }}
        </Button>
        <button
          v-if="status.has_recovery"
          type="button"
          class="ml-auto text-[11px] text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
          @click="useRecovery = !useRecovery; unlockError = ''"
        >
          {{ useRecovery ? t('secretNotes.unlock.usePassword') : t('secretNotes.unlock.useRecovery') }}
        </button>
      </div>
    </form>
  </div>

  <!-- Unlocked -->
  <template v-else>
    <aside class="flex w-[280px] shrink-0 flex-col border-r border-border/60 bg-sidebar">
      <div class="shrink-0 space-y-2 border-b border-border/60 px-3 py-3">
        <div class="flex items-center gap-1.5">
          <div class="relative min-w-0 flex-1">
            <Search
              class="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground"
              :stroke-width="1.75"
            />
            <Input v-model="search" class="pl-8 pr-7" :placeholder="t('secretNotes.search')" />
            <button
              v-if="search"
              type="button"
              class="absolute right-2 top-1/2 -translate-y-1/2 cursor-pointer text-muted-foreground hover:text-foreground"
              :title="t('item.clearSearch')"
              @click="search = ''"
            >
              <X class="h-3.5 w-3.5" :stroke-width="2" />
            </button>
          </div>
          <Button
            variant="outline"
            size="icon-sm"
            class="shrink-0"
            :title="t('secretNotes.lock')"
            :aria-label="t('secretNotes.lock')"
            @click="lockNow"
          >
            <Lock class="h-3.5 w-3.5" :stroke-width="1.75" />
          </Button>
        </div>
        <div class="flex items-center gap-1.5">
          <div class="min-w-0 flex-1">
            <AppSelect v-model="projectFilter" :options="filterOptions" size="sm">
              <template #leading>
                <Folder class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.75" />
              </template>
            </AppSelect>
          </div>
          <Button size="sm" class="shrink-0" @click="startCreate">
            <Plus class="h-3.5 w-3.5" :stroke-width="2" />
            {{ t('secretNotes.new') }}
          </Button>
        </div>
      </div>

      <div class="min-h-0 flex-1 overflow-y-auto px-2 py-2">
        <!-- Notes of another vault (sync / backup): merge with its password -->
        <form
          v-if="status.legacy_count"
          class="mb-2 space-y-1.5 rounded-md border border-amber-500/40 bg-amber-500/5 p-2"
          @submit.prevent="merge"
        >
          <p class="text-[11px]">{{ t('secretNotes.merge.help', { n: status.legacy_count }) }}</p>
          <div class="flex items-center gap-1.5">
            <Input v-model="mergePw" type="password" autocomplete="off" class="h-7" :placeholder="t('secretNotes.merge.password')" />
            <Button type="submit" size="xs" :disabled="merging || !mergePw">
              <Loader2 v-if="merging" class="h-3 w-3 animate-spin" :stroke-width="1.75" />
              {{ t('secretNotes.merge.button') }}
            </Button>
          </div>
        </form>
        <p v-if="status.pending_count" class="mb-2 px-1 text-[11px] text-muted-foreground">
          {{ t('secretNotes.pending', { n: status.pending_count }) }}
        </p>

        <p v-if="!notes.length" class="px-1 py-6 text-center text-[11px] text-muted-foreground">
          {{ t('secretNotes.empty') }}
        </p>
        <p v-else-if="!visible.length" class="px-1 py-6 text-center text-[11px] text-muted-foreground">
          {{ t('secretNotes.noMatch') }}
        </p>
        <button
          v-for="n in visible"
          :key="n.id"
          type="button"
          class="mb-0.5 flex w-full cursor-pointer flex-col items-start gap-0.5 rounded-md px-2 py-1.5 text-left transition-colors"
          :class="n.id === selectedId ? 'bg-accent text-foreground' : 'hover:bg-accent/60'"
          @click="select(n.id)"
        >
          <span class="flex w-full min-w-0 items-center gap-1.5 text-xs font-medium">
            <Lock class="h-3 w-3 shrink-0 text-muted-foreground" :stroke-width="1.75" />
            <span class="truncate">{{ n.title || t('secretNotes.untitled') }}</span>
          </span>
          <span class="flex w-full min-w-0 gap-1.5 pl-[18px] text-[10px] text-muted-foreground">
            <span v-if="n.project_id" class="truncate">{{ projectNames[n.project_id] ?? t('item.noProject') }}</span>
            <span class="ml-auto shrink-0">{{ relativeAge(n.updated_at) }}</span>
          </span>
        </button>
      </div>

      <!-- Auto-lock note + change password -->
      <div class="shrink-0 space-y-2 border-t border-border/60 px-3 py-2">
        <form v-if="changingPassword" class="space-y-1.5" @submit.prevent="changePassword">
          <p class="text-[11px] font-medium">
            {{ mustChangePassword ? t('secretNotes.password.afterRecovery') : t('secretNotes.password.title') }}
          </p>
          <Input
            v-if="!mustChangePassword"
            v-model="currentPw"
            type="password"
            autocomplete="current-password"
            class="h-7"
            :placeholder="t('secretNotes.password.current')"
          />
          <Input v-model="newPw" type="password" autocomplete="new-password" class="h-7" :placeholder="t('secretNotes.setup.password')" />
          <Input v-model="newPw2" type="password" autocomplete="new-password" class="h-7" :placeholder="t('secretNotes.setup.confirm')" />
          <p v-if="newPwProblem" class="text-[11px] text-destructive">{{ t(`secretNotes.password.${newPwProblem}`) }}</p>
          <div class="flex items-center gap-1.5">
            <Button
              type="submit"
              size="xs"
              :disabled="savingPassword || !!passwordProblem(newPw, newPw2) || (!mustChangePassword && !currentPw)"
            >
              {{ t('secretNotes.password.save') }}
            </Button>
            <Button variant="ghost" size="xs" @click="cancelPasswordChange">{{ t('secretNotes.cancel') }}</Button>
          </div>
        </form>
        <div v-else class="flex items-center gap-2 text-[10px] text-muted-foreground">
          <span class="min-w-0 flex-1">{{ t('secretNotes.autoLock', { n: Math.round(status.auto_lock_secs / 60) }) }}</span>
          <button type="button" class="shrink-0 underline-offset-2 hover:text-foreground hover:underline" @click="changingPassword = true">
            {{ t('secretNotes.password.title') }}
          </button>
        </div>
      </div>
    </aside>

    <!-- Editor (new note, or editing the selected one) -->
    <section v-if="editing" class="flex min-w-0 flex-1 flex-col gap-2 p-4">
      <Input
        v-model="draftTitle"
        autocomplete="off"
        spellcheck="false"
        :placeholder="t('secretNotes.titlePlaceholder')"
      />
      <Textarea
        v-model="draftContent"
        class="min-h-0 flex-1 resize-none font-mono"
        autocomplete="off"
        autocorrect="off"
        autocapitalize="off"
        spellcheck="false"
        :placeholder="t('secretNotes.contentPlaceholder')"
        @keydown.meta.enter.prevent="save"
        @keydown.ctrl.enter.prevent="save"
        @keydown.esc.prevent="cancelEdit"
      />
      <div class="flex items-center gap-2">
        <span v-if="tooLarge" class="text-[11px] text-destructive">{{ t('secretNotes.errors.too_large') }}</span>
        <Button variant="outline" size="xs" class="ml-auto" @click="cancelEdit">{{ t('secretNotes.cancel') }}</Button>
        <Button size="xs" :disabled="saving || tooLarge || (!draftTitle.trim() && !draftContent.trim())" @click="save">
          <Loader2 v-if="saving" class="h-3 w-3 animate-spin" :stroke-width="1.75" />
          {{ t('secretNotes.save') }}
        </Button>
      </div>
    </section>

    <!-- Detail -->
    <section v-else-if="selected" class="flex min-w-0 flex-1 flex-col">
      <div class="flex shrink-0 items-center gap-2 border-b border-border/60 px-4 py-2.5">
        <Lock class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
        <h2 class="min-w-0 flex-1 truncate text-sm font-semibold">{{ selected.title || t('secretNotes.untitled') }}</h2>
        <div class="w-40 shrink-0">
          <AppSelect
            :model-value="selected.project_id ?? ''"
            :options="projectOptions"
            size="sm"
            @update:model-value="setProject"
          />
        </div>
        <Button variant="outline" size="icon-sm" :title="t('secretNotes.edit')" :aria-label="t('secretNotes.edit')" @click="startEdit">
          <Pencil class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
        <Button variant="destructive-ghost" size="icon-sm" :title="t('common.delete')" :aria-label="t('common.delete')" @click="remove">
          <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
      </div>
      <div class="min-h-0 flex-1 overflow-y-auto p-4">
        <div class="mb-3 flex items-center gap-2">
          <Button v-if="content === null" variant="outline" size="xs" :disabled="revealing" @click="reveal">
            <component :is="revealing ? Loader2 : Eye" class="h-3 w-3" :class="{ 'animate-spin': revealing }" :stroke-width="1.75" />
            {{ t('secretNotes.reveal') }}
          </Button>
          <Button v-else variant="outline" size="xs" @click="hideContent">
            <EyeOff class="h-3 w-3" :stroke-width="1.75" />
            {{ t('secretNotes.hide') }}
          </Button>
          <Button variant="outline" size="xs" @click="copy">
            <Copy class="h-3 w-3" :stroke-width="1.75" />
            {{ t('secretNotes.copy') }}
          </Button>
          <span class="ml-auto text-[10px] text-muted-foreground">{{ relativeAge(selected.updated_at) }}</span>
        </div>
        <pre
          v-if="content !== null"
          class="whitespace-pre-wrap break-words rounded-md border border-border/60 bg-muted/30 p-3 font-mono text-xs select-text"
        >{{ content }}</pre>
        <p v-else class="select-none rounded-md border border-dashed border-border/60 p-3 font-mono text-xs text-muted-foreground">
          ••••••••••••
        </p>
        <p class="mt-2 text-[10px] text-muted-foreground">{{ t('secretNotes.revealHint', { n: REVEAL_SECS }) }}</p>
      </div>
    </section>

    <div v-else class="flex min-w-0 flex-1 items-center justify-center p-6 text-center">
      <div>
        <div class="mx-auto mb-3 flex h-10 w-10 items-center justify-center rounded-xl bg-muted">
          <ShieldCheck class="h-5 w-5 text-muted-foreground" :stroke-width="1.5" />
        </div>
        <p class="max-w-xs text-xs text-muted-foreground">{{ t('secretNotes.noSelection') }}</p>
      </div>
    </div>
  </template>
</template>
