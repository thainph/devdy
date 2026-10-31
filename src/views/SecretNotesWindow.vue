<script setup lang="ts">
// THE Secret notes window (opened via lib/secretNotesWindow,
// `?secretNotesWindow=1[&projectId=][&id=]`) — the vault of notes sealed with a
// password (backend src-tauri/src/secret_vault.rs), laid out like the Notes /
// Todos windows:
//
//   ┌ (native OS titlebar only: "Secret notes") ─────────────────────────────┐
//   ├ sidebar: search · pin · lock,          │ detail header: title / meta · │
//   │ project filter + New,                  │ [project] | 👁 ⧉ ✎ ⋯           │
//   │ notes grouped by project,              │ body: hidden content (Reveal) │
//   │ auto-lock · Change password            │ or the inline editor          │
//   └────────────────────────────────────────┴───────────────────────────────┘
//
// No vault → setup (then the recovery key, shown once); locked → unlock with the
// password or the recovery key (which then asks for a new password). Secret notes
// are not in the notes table, so no MCP tool, agent or remote controller reads
// them, and the backend refuses every other webview (this window only).
//
// A note's content is fetched only on Reveal / Edit and hidden again after
// REVEAL_SECS or when the window loses focus; Copy happens in the backend (cleared
// from the clipboard after 30 s). Content never goes to localStorage, the URL,
// toasts or logs. Typing in the editor pushes the backend's idle auto-lock back
// (vaultTouch); when the vault locks anyway, open drafts are dropped.
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { Eye, ShieldCheck } from 'lucide-vue-next'
import { Button, ConfirmModal, Input, Modal, Textarea, ToastHost } from '@/components/ui'
import SecretNotesSidebar from '@/components/secretNotes/SecretNotesSidebar.vue'
import SecretNoteHeader from '@/components/secretNotes/SecretNoteHeader.vue'
import VaultGate from '@/components/secretNotes/VaultGate.vue'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { useFloatingWindow } from '@/composables/useFloatingWindow'
import { useProjectsStore } from '@/stores/projects'
import { refreshTray } from '@/lib/tray'
import { SECRET_NOTES_WINDOW_SELECT, type SecretNotesWindowSelect } from '@/lib/secretNotesWindow'
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
  vaultStatus,
  vaultTouch,
  type SecretNoteMeta,
  type VaultStatus,
} from '@/lib/secretVault'

const { t } = useI18n()
const { toast } = useToast()
const { confirm } = useConfirm()
const projects = useProjectsStore()
const { pinned, togglePin } = useFloatingWindow()

const params = new URLSearchParams(window.location.search)
const initialId = params.get('id') || null
/** The project the window was opened from: the default filter and "+ New" project. */
const contextProjectId = ref<string | null>(params.get('projectId') || null)

function errText(e: unknown): string {
  const { code, wait } = vaultError(e)
  return code ? t(`secretNotes.errors.${code}`, { n: wait ?? 0 }) : String(e)
}

// ── Vault state ──────────────────────────────────────────────────────────────

const status = ref<VaultStatus | null>(null)
const notes = ref<SecretNoteMeta[]>([])
const loadError = ref('')
/** Shown once after setup / a password change, until acknowledged. */
const recoveryKey = ref<string | null>(null)

const unlocked = computed(() => !!status.value?.unlocked && !recoveryKey.value)

async function refresh() {
  try {
    status.value = await vaultStatus()
    notes.value = status.value.unlocked ? await secretNotesList() : []
    loadError.value = ''
  } catch (e) {
    loadError.value = errText(e)
  }
  if (!status.value?.unlocked) forgetOpen()
  else if (selectedId.value && !notes.value.some((n) => n.id === selectedId.value)) dropSelection()
}

/** Drop everything sensitive the window holds (on lock). */
function forgetOpen() {
  hideContent()
  clearDraft()
  selectedId.value = null
  // Not the recovery key: it must survive a lock until acknowledged.
  closePasswordDialog(true)
  mergePassword.value = ''
}

async function lockNow() {
  await vaultLock().catch(() => {})
  await refresh()
}

function onCreated(key: string) {
  recoveryKey.value = key
  void refresh()
}

async function onUnlocked(viaRecovery: boolean) {
  await refresh()
  // Opened with the recovery key: a new password is due before anything else.
  if (viaRecovery) openPasswordDialog(true)
  else if (!selectedId.value) selectFirst()
}

// ── Sidebar: search + project filter ─────────────────────────────────────────

const NO_PROJECT = '__none__'
const search = ref('')
/** '' = all projects, NO_PROJECT = notes without one, else a project id. */
const projectFilter = ref<string>(contextProjectId.value ?? '')

const visible = computed(() => filterSecretNotes(notes.value, search.value, projectFilter.value, NO_PROJECT))

const projectNames = computed<Record<string, string>>(() =>
  Object.fromEntries(projects.projects.map((p) => [p.id, p.name])),
)

const projectOptions = computed(() => {
  const ids = new Set<string>()
  for (const n of notes.value) if (n.project_id) ids.add(n.project_id)
  if (projectFilter.value && projectFilter.value !== NO_PROJECT) ids.add(projectFilter.value)
  const named = [...ids]
    .map((id) => ({ value: id, label: projectNames.value[id] ?? id }))
    .sort((a, b) => a.label.localeCompare(b.label))
  return [
    { value: '', label: t('item.allProjects') },
    ...named,
    { value: NO_PROJECT, label: t('item.noProject') },
  ]
})

// ── Selection ──────────────────────────────────────────────────────────────────

const selectedId = ref<string | null>(null)
const selected = computed(() => notes.value.find((n) => n.id === selectedId.value) ?? null)

async function select(id: string | null) {
  if (id === selectedId.value && !creatingNew.value) return
  if (!(await confirmDiscard())) return
  clearDraft()
  hideContent()
  selectedId.value = id
  if (id) revealRow(id)
}

/** Make a row visible under the filters (a note asked for from elsewhere). */
function revealRow(id: string) {
  const n = notes.value.find((x) => x.id === id)
  if (!n || filterSecretNotes([n], search.value, projectFilter.value, NO_PROJECT).length) return
  search.value = ''
  projectFilter.value = ''
}

function selectFirst() {
  const wanted = initialId && notes.value.some((n) => n.id === initialId) ? initialId : null
  selectedId.value = wanted ?? visible.value[0]?.id ?? null
}

/** The selected note vanished (deleted here or on another synced device). */
function dropSelection() {
  if (editing.value && !creatingNew.value) clearDraft()
  hideContent()
  selectedId.value = visible.value[0]?.id ?? null
}

// ── Reveal / copy ──────────────────────────────────────────────────────────────

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

// ── Edit / create ─────────────────────────────────────────────────────────────

const editing = ref(false)
/** Writing a new note (not yet in the vault). */
const creatingNew = ref(false)
const draftTitle = ref('')
const draftContent = ref('')
/** Project of the new note. */
const draftProject = ref<string | null>(null)
/** What the editor started from, the yardstick for `dirty`. */
let storedTitle = ''
let storedContent = ''
const saving = ref(false)
const titleRef = ref<InstanceType<typeof Input> | null>(null)
const contentRef = ref<InstanceType<typeof Textarea> | null>(null)

const tooLarge = computed(() => contentBytes(draftContent.value) > MAX_SECRET_CONTENT)
const canSave = computed(
  () => !tooLarge.value && !!(draftTitle.value.trim() || draftContent.value.trim()),
)
const dirty = computed(
  () => editing.value && (draftTitle.value !== storedTitle || draftContent.value !== storedContent),
)

function clearDraft() {
  editing.value = false
  creatingNew.value = false
  draftTitle.value = ''
  draftContent.value = ''
  storedTitle = ''
  storedContent = ''
}

/** Leaving unsaved edits takes a confirm; true = go ahead. */
async function confirmDiscard(): Promise<boolean> {
  if (!dirty.value) return true
  return confirm({
    title: t('item.confirm.discardTitle'),
    message: t('item.confirm.discardMessage'),
    confirmLabel: t('item.confirm.discard'),
  })
}

function focusEditor() {
  nextTick(() => {
    const el = draftTitle.value ? contentRef.value?.$el : titleRef.value?.$el
    ;(el as HTMLElement | undefined)?.focus()
  })
}

async function startCreate() {
  if (!(await confirmDiscard())) return
  clearDraft()
  hideContent()
  creatingNew.value = true
  draftProject.value =
    projectFilter.value && projectFilter.value !== NO_PROJECT ? projectFilter.value : contextProjectId.value
  editing.value = true
  focusEditor()
}

async function startEdit() {
  const id = selectedId.value
  if (!id || editing.value) return
  try {
    const text = await secretNoteReveal(id)
    if (selectedId.value !== id) return
    hideContent()
    storedTitle = selected.value?.title ?? ''
    storedContent = text
    draftTitle.value = storedTitle
    draftContent.value = storedContent
    creatingNew.value = false
    editing.value = true
    focusEditor()
  } catch (e) {
    toast.error(errText(e))
    await refresh()
  }
}

async function cancelEdit() {
  if (!(await confirmDiscard())) return
  clearDraft()
}

async function save() {
  if (saving.value || !canSave.value) return
  saving.value = true
  try {
    if (creatingNew.value) {
      const m = await secretNoteCreate(draftTitle.value.trim(), draftContent.value, draftProject.value)
      selectedId.value = m.id
    } else if (selectedId.value) {
      await secretNoteUpdate(selectedId.value, draftTitle.value.trim(), draftContent.value)
    }
    clearDraft()
    await refresh()
  } catch (e) {
    toast.error(errText(e))
    if (vaultError(e).code === 'vault_locked') await refresh()
  } finally {
    saving.value = false
  }
}

function onEditorKeydown(e: KeyboardEvent) {
  if (e.isComposing || e.keyCode === 229) return
  if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
    e.preventDefault()
    void save()
  } else if (e.key === 'Escape') {
    e.preventDefault()
    void cancelEdit()
  }
}

// Typing keeps the vault open: the backend locks after its idle time without calls.
let lastTouch = 0
watch([draftTitle, draftContent], () => {
  if (!editing.value || Date.now() - lastTouch < 30_000) return
  lastTouch = Date.now()
  vaultTouch().catch(() => void refresh())
})

async function setProject(projectId: string | null) {
  if (creatingNew.value) {
    draftProject.value = projectId
    return
  }
  const id = selectedId.value
  if (!id || projectId === (selected.value?.project_id ?? null)) return
  try {
    await secretNoteSetProject(id, projectId)
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
  // The next row takes the selection.
  const rows = visible.value
  const i = rows.findIndex((r) => r.id === m.id)
  const next = rows[i + 1]?.id ?? rows[i - 1]?.id ?? null
  try {
    await secretNoteDelete(m.id)
    hideContent()
    clearDraft()
    selectedId.value = next
    await refresh()
  } catch (e) {
    toast.error(errText(e))
  }
}

// ── Merge older vaults ─────────────────────────────────────────────────────────

const mergePassword = ref('')
const merging = ref(false)

async function merge() {
  if (merging.value || !mergePassword.value) return
  merging.value = true
  try {
    const n = await vaultMergeOld(mergePassword.value)
    mergePassword.value = ''
    toast.success(t('secretNotes.merge.done', { n }))
    await refresh()
  } catch (e) {
    toast.error(errText(e))
  } finally {
    merging.value = false
  }
}

// ── Change password (dialog) ───────────────────────────────────────────────────

const passwordDialog = ref(false)
/** Unlocked with the recovery key: a new password is due (no current one, no closing). */
const mustChangePassword = ref(false)
const currentPw = ref('')
const newPw = ref('')
const newPw2 = ref('')
const savingPassword = ref(false)
const newPwProblem = computed(() => (newPw.value ? passwordProblem(newPw.value, newPw2.value) : null))

function openPasswordDialog(forced = false) {
  mustChangePassword.value = forced
  passwordDialog.value = true
}

/** `force` also closes a mandatory dialog (the vault locked under it). */
function closePasswordDialog(force = false) {
  if (mustChangePassword.value && !force) return
  currentPw.value = ''
  newPw.value = ''
  newPw2.value = ''
  passwordDialog.value = false
  mustChangePassword.value = false
}

async function changePassword() {
  if (savingPassword.value || passwordProblem(newPw.value, newPw2.value)) return
  if (!mustChangePassword.value && !currentPw.value) return
  savingPassword.value = true
  try {
    const key = await vaultChangePassword(mustChangePassword.value ? null : currentPw.value, newPw.value)
    closePasswordDialog(true)
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

// ── Window ───────────────────────────────────────────────────────────────────

const windowTitle = computed(() => t('secretNotes.window.title'))
watch(
  windowTitle,
  async (value) => {
    document.title = value
    try {
      await getCurrentWindow().setTitle(value)
    } catch {
      /* outside the Tauri shell */
    }
    void refreshTray()
  },
  { immediate: true },
)

const unlisten: UnlistenFn[] = []
/** The close was confirmed: let the next close request through. */
let closing = false

onMounted(async () => {
  void projects.fetchProjects()
  try {
    unlisten.push(
      await onVaultChanged(() => void refresh()),
      // Reopened from the app: re-point the project scope and / or select a note.
      await listen<SecretNotesWindowSelect>(SECRET_NOTES_WINDOW_SELECT, (e) => {
        const p = e.payload
        if (!p) return
        if (p.projectId !== undefined) {
          contextProjectId.value = p.projectId ?? null
          projectFilter.value = p.projectId ?? ''
        }
        if (p.id) void refresh().then(() => select(p.id ?? null))
      }),
      // Revealed content hides as soon as the window loses focus.
      await getCurrentWindow().onFocusChanged(({ payload: focused }) => {
        if (!focused) hideContent()
      }),
      // Closing with an unsaved draft asks first.
      await getCurrentWindow().onCloseRequested(async (e) => {
        if (closing || !dirty.value) return
        e.preventDefault()
        if (!(await confirmDiscard())) return
        closing = true
        await getCurrentWindow().destroy()
      }),
    )
  } catch {
    /* running outside the Tauri shell */
  }
  await refresh()
  if (status.value?.unlocked) selectFirst()
})

onBeforeUnmount(() => {
  unlisten.forEach((off) => off())
  forgetOpen()
})
</script>

<template>
  <div class="flex h-screen w-screen overflow-hidden bg-background text-foreground">
    <VaultGate
      v-if="!unlocked"
      :status="status"
      :load-error="loadError"
      :recovery-key="recoveryKey"
      @created="onCreated"
      @unlocked="onUnlocked"
      @ack-recovery="recoveryKey = null; selectFirst()"
    />

    <template v-else-if="status">
      <SecretNotesSidebar
        v-model:search="search"
        v-model:project="projectFilter"
        v-model:merge-password="mergePassword"
        :notes="visible"
        :empty-vault="notes.length === 0"
        :selected-id="creatingNew ? null : selectedId"
        :project-options="projectOptions"
        :project-names="projectNames"
        :pinned="pinned"
        :legacy-count="status.legacy_count"
        :pending-count="status.pending_count"
        :merging="merging"
        :auto-lock-minutes="Math.round(status.auto_lock_secs / 60)"
        @select="select"
        @add="startCreate"
        @toggle-pin="togglePin"
        @lock="lockNow"
        @merge="merge"
        @change-password="openPasswordDialog()"
      />

      <section v-if="creatingNew || selected" class="flex min-w-0 flex-1 flex-col">
        <SecretNoteHeader
          :note="creatingNew ? null : selected"
          :project-id="creatingNew ? draftProject : selected?.project_id ?? null"
          :editing="editing"
          :can-save="canSave"
          :saving="saving"
          :revealed="content !== null"
          :revealing="revealing"
          @edit="startEdit"
          @cancel-edit="cancelEdit"
          @save="save"
          @set-project="setProject"
          @reveal="reveal"
          @hide="hideContent"
          @copy="copy"
          @delete="remove"
        />

        <!-- Editing: the fields fill the pane. -->
        <div v-if="editing" class="flex min-h-0 flex-1 flex-col px-5 py-4" @keydown="onEditorKeydown">
          <Input
            ref="titleRef"
            v-model="draftTitle"
            class="mb-2 shrink-0 font-medium"
            autocomplete="off"
            spellcheck="false"
            :placeholder="t('item.titlePlaceholder')"
          />
          <Textarea
            ref="contentRef"
            v-model="draftContent"
            class="min-h-[8rem] flex-1 resize-none font-mono leading-relaxed"
            autocomplete="off"
            autocorrect="off"
            autocapitalize="off"
            spellcheck="false"
            :placeholder="t('secretNotes.contentPlaceholder')"
          />
          <p class="mt-2 shrink-0 text-[11px]" :class="tooLarge ? 'text-destructive' : 'text-muted-foreground'">
            {{ tooLarge ? t('secretNotes.errors.too_large') : t('secretNotes.editHint', { save: '⌘/Ctrl+Enter', esc: 'Esc' }) }}
          </p>
        </div>

        <!-- Reading: hidden until revealed. -->
        <div v-else class="min-h-0 flex-1 overflow-y-auto px-5 py-4">
          <pre
            v-if="content !== null"
            class="select-text whitespace-pre-wrap break-words font-mono text-sm leading-relaxed"
          >{{ content }}</pre>
          <button
            v-else
            type="button"
            class="flex w-full cursor-pointer flex-col items-center gap-1.5 rounded-md border border-dashed border-border/70 px-4 py-8 text-muted-foreground transition-colors hover:border-primary/50 hover:bg-accent/30 hover:text-foreground"
            :disabled="revealing"
            @click="reveal"
          >
            <Eye class="h-4 w-4" :stroke-width="1.75" />
            <span class="font-mono text-xs tracking-widest">••••••••••••</span>
            <span class="text-[11px]">{{ t('secretNotes.clickToReveal') }}</span>
          </button>
          <p class="mt-3 text-[11px] text-muted-foreground">{{ t('secretNotes.revealHint', { n: REVEAL_SECS }) }}</p>
        </div>
      </section>

      <div v-else class="flex min-w-0 flex-1 items-center justify-center p-6">
        <div class="text-center">
          <div class="mx-auto mb-3 flex h-10 w-10 items-center justify-center rounded-xl bg-muted">
            <ShieldCheck class="h-5 w-5 text-muted-foreground" :stroke-width="1.5" />
          </div>
          <p class="max-w-xs text-xs text-muted-foreground">{{ t('secretNotes.noSelection') }}</p>
        </div>
      </div>
    </template>

    <!-- Change password: re-encrypts the vault under a new key. -->
    <Modal
      :open="passwordDialog"
      size="sm"
      :closable="!mustChangePassword"
      :dismiss-on-overlay="false"
      :title="mustChangePassword ? t('secretNotes.password.afterRecovery') : t('secretNotes.password.title')"
      @close="closePasswordDialog()"
    >
      <form class="space-y-2 p-4" @submit.prevent="changePassword">
        <p class="text-xs text-muted-foreground">{{ t('secretNotes.password.help') }}</p>
        <Input
          v-if="!mustChangePassword"
          v-model="currentPw"
          type="password"
          autocomplete="current-password"
          :placeholder="t('secretNotes.password.current')"
        />
        <Input v-model="newPw" type="password" autocomplete="new-password" :placeholder="t('secretNotes.setup.password')" />
        <Input v-model="newPw2" type="password" autocomplete="new-password" :placeholder="t('secretNotes.setup.confirm')" />
        <p v-if="newPwProblem" class="text-[11px] text-destructive">{{ t(`secretNotes.password.${newPwProblem}`) }}</p>
        <div class="flex items-center justify-end gap-2 pt-2">
          <Button v-if="!mustChangePassword" variant="outline" size="sm" @click="closePasswordDialog()">
            {{ t('common.cancel') }}
          </Button>
          <Button
            type="submit"
            size="sm"
            :disabled="savingPassword || !!passwordProblem(newPw, newPw2) || (!mustChangePassword && !currentPw)"
          >
            {{ t('secretNotes.password.save') }}
          </Button>
        </div>
      </form>
    </Modal>

    <!-- The pop-out doesn't mount the main app's dialog/toast hosts. -->
    <ConfirmModal />
    <ToastHost />
  </div>
</template>
