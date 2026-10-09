<script setup lang="ts">
// THE todo / note window — bare, chrome-less host for both writing modes
// (opened via lib/itemWindow, `?itemWindow=1&mode=create|edit&…`).
//
// Every write to a todo or a note goes through here: the app has no in-place
// editor, no detail drawer and no edit modal, so an item looks and behaves the
// same no matter where the user opened it from. The main window keeps whatever
// it was showing.
//
// It is a self-contained WRITER: it saves straight to the shared SQLite DB
// through the Pinia stores, then broadcasts ITEM_CHANGED so the list drawer in
// the main window (a different webview, with its own store copy) refetches.
//
//   create — QuickCaptureForm, with Todo/Note tabs; stays open so several
//            thoughts can be captured in a row.
//   edit   — ItemDetail, with a preview/edit toggle, delete and save.
//
// Edit mode shares the capture window's chrome (views/CaptureWindow.vue): the
// same titlebar, the same action bar — reading: the project on the left, Mark
// done (todo) / Copy markdown (note) + Edit + the ⋯ menu (Copy ID, Delete) on
// the right; writing: Cancel + Save — and the same shortcuts: E edits, F2 renames a note's title,
// ⌘/Ctrl+Enter saves.
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { emit, listen, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { Check, ListTodo, StickyNote, Undo2 } from 'lucide-vue-next'
import { Button, ConfirmModal, ToastHost } from '@/components/ui'
import DetailWindowTitlebar from '@/components/DetailWindowTitlebar.vue'
import DetailWindowFooter from '@/components/DetailWindowFooter.vue'
import CopyMarkdownButton from '@/components/CopyMarkdownButton.vue'
import ProjectSelect from '@/components/ProjectSelect.vue'
import WindowPinButton from '@/components/WindowPinButton.vue'
import QuickCaptureForm from '@/components/QuickCaptureForm.vue'
import ItemDetail from '@/components/ItemDetail.vue'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { useQuickCapture } from '@/composables/useQuickCapture'
import { useFloatingWindow } from '@/composables/useFloatingWindow'
import { useNotesStore, type Note } from '@/stores/notes'
import { useTodosStore, type Todo } from '@/stores/todos'
import {
  ITEM_CHANGED,
  ITEM_WINDOW_PREFILL,
  ITEM_WINDOW_READY,
  ITEM_WINDOW_SET_TAB,
  type ItemKind,
  type ItemPrefill,
  type ItemWindowMode,
  type ItemWindowSetTab,
} from '@/lib/itemWindow'
import { refreshTray } from '@/lib/tray'
import { firstLine } from '@/lib/itemText'

const { t } = useI18n()
const { toast } = useToast()
const { confirm, state: confirmState } = useConfirm()
const notes = useNotesStore()
const todos = useTodosStore()
// This is its own webview, so it gets its own copy of the capture state.
const { tab, openCapture, setContext, adoptDraft } = useQuickCapture()
// Floats above other apps while focused; pin to keep it on top deliberately.
const { pinned, togglePin } = useFloatingWindow()

const params = new URLSearchParams(window.location.search)
const mode: ItemWindowMode = params.get('mode') === 'edit' ? 'edit' : 'create'
const initialKind: ItemKind = params.get('kind') === 'note' ? 'note' : 'todo'
const itemId = params.get('id') ?? ''

const loading = ref(mode === 'edit')
const reloading = ref(false)
// Edit mode opens on the item as it READS; writing is a deliberate step from
// there, and saving returns here. Create mode has nothing to preview.
const previewing = ref(mode === 'edit')
const formRef = ref<InstanceType<typeof QuickCaptureForm> | null>(null)
const detailRef = ref<InstanceType<typeof ItemDetail> | null>(null)

let unlisten: UnlistenFn[] = []

// Edit mode reads straight from the store, so a save (applied optimistically)
// shows without a refetch.
const item = computed<Note | Todo | null>(() => {
  if (mode !== 'edit') return null
  return initialKind === 'note'
    ? notes.notes.find((n) => n.id === itemId) ?? null
    : todos.todos.find((x) => x.id === itemId) ?? null
})
const missing = computed(() => mode === 'edit' && !loading.value && !item.value)
const done = computed(() => (initialKind === 'todo' ? !!(item.value as Todo | null)?.done : false))

// Edit mode only: create mode has no titlebar (its tabs carry the identity).
//
// A todo has no title, so it is labelled by its own words: the first sentence
// of its first line (the full line is in the tooltip). Its id — what the AI
// side addresses it by — stays one click away under ⋯ → Copy ID.
const todoLine = computed(() => firstLine((item.value as Todo | null)?.text ?? ''))
const heading = computed(() => {
  if (initialKind === 'todo') return firstSentence(todoLine.value) || t('item.editTodoHeading')
  return (item.value as Note | null)?.title?.trim() || t('item.editNoteHeading')
})

/** Up to the first sentence end (Latin or CJK punctuation); the whole line if none. */
function firstSentence(line: string): string {
  return line.match(/^.*?[.!?。！？](?=\s|$)/)?.[0] ?? line
}

// The OS window title doubles as this window's row label in the menu-bar
// switcher (lib/tray.ts), so keep it on something a person recognizes: the kind
// plus the item's own words — a note's title, a todo's first line (its id would
// just be a UUID). Create mode follows the active tab. Updates once the item
// loads, and refreshes the switcher on every change so the row follows a rename.
const windowTitle = computed(() => {
  if (mode !== 'edit') return tab.value === 'note' ? t('item.newNote') : t('item.newTodo')
  if (initialKind === 'note') {
    const note = item.value as Note | null
    const name =
      note?.title?.trim() || firstLine(note?.content ?? '') || t('item.editNoteHeading')
    return t('tray.noteItem', { name })
  }
  const name = firstLine((item.value as Todo | null)?.text ?? '') || t('item.editTodoHeading')
  return t('tray.todoItem', { name })
})
watch(
  windowTitle,
  async (title) => {
    document.title = title
    // Setting document.title alone does NOT update the OS window title in Tauri,
    // and the menu-bar switcher reads the OS title (lib/tray.ts). So push it to the
    // window explicitly — the same way the session pop-out does (App.vue) — or the
    // titlebar and the switcher row stay stuck on the static creation-time title.
    try {
      await getCurrentWindow().setTitle(title)
    } catch {
      /* outside the Tauri shell */
    }
    void refreshTray()
  },
  { immediate: true },
)

const saveLabel = computed(() =>
  mode === 'edit'
    ? t('common.save')
    : tab.value === 'todo'
      ? t('item.addTodo')
      : t('item.saveNote'),
)

const TABS: { key: ItemKind; labelKey: string; icon: typeof ListTodo }[] = [
  { key: 'todo', labelKey: 'item.tabTodo', icon: ListTodo },
  { key: 'note', labelKey: 'item.tabNote', icon: StickyNote },
]

function announce() {
  emit(ITEM_CHANGED, { kind: mode === 'edit' ? initialKind : tab.value })
}

async function loadItem() {
  loading.value = true
  try {
    if (initialKind === 'note') await notes.fetchNotes()
    else await todos.fetchTodos()
  } finally {
    loading.value = false
  }
}

// Pull the item's latest content back from the DB — the analog of the file
// viewer's reload button, for when the AI side (or another window) changed it
// while this one was open. Unlike loadItem() it never flips to the skeleton, so
// the preview / fields stay put while refetching. The read view (storedContent)
// updates on its own; the edit draft is re-synced to the fresh values too —
// except when the user has unsaved edits in progress, which must not be lost.
async function reload() {
  if (mode !== 'edit' || reloading.value) return
  reloading.value = true
  try {
    if (initialKind === 'note') await notes.fetchNotes()
    else await todos.fetchTodos()
    await nextTick()
    const detail = detailRef.value
    if (detail && !(!previewing.value && detail.dirty)) detail.reset()
  } finally {
    reloading.value = false
  }
}

// The id is what the AI side addresses an item by (notes_read, todos_done, …),
// so hand it over in one click instead of making the user retype it.
const copiedId = ref(false)
async function copyId() {
  if (!itemId) return
  try {
    await navigator.clipboard.writeText(itemId)
    copiedId.value = true
    setTimeout(() => { copiedId.value = false }, 1500)
    toast.success(t('item.idCopied'))
  } catch { /* clipboard unavailable */ }
}

/** The footer's project picker (read view) writes straight away. */
async function onProjectChange(next: string | null) {
  if (mode !== 'edit' || !item.value || next === item.value.project_id) return
  if (initialKind === 'note') await notes.setProject(itemId, next)
  else await todos.setProject(itemId, next)
  announce()
}

async function toggleDone() {
  if (mode !== 'edit' || initialKind !== 'todo' || !item.value) return
  await todos.toggle(itemId)
  announce()
}

async function remove() {
  if (mode !== 'edit' || !item.value) return
  const isNote = initialKind === 'note'
  if (
    !(await confirm({
      title: isNote ? t('item.confirm.deleteNoteTitle') : t('item.confirm.deleteTodoTitle'),
      message: isNote ? t('item.confirm.deleteNoteMessage') : t('item.confirm.deleteTodoMessage'),
      confirmLabel: t('common.delete'),
    }))
  ) {
    return
  }
  if (isNote) await notes.remove(itemId)
  else await todos.remove(itemId)
  announce()
  // Nothing left to edit: the window's job is done.
  getCurrentWindow().close().catch(() => { /* already closing */ })
}

function onSaved() {
  announce()
  if (mode !== 'edit') return
  toast.success(t('item.saved'))
  // Saving is also how the user leaves the fields: back to reading.
  previewing.value = true
}

/**
 * The Save button, which doubles as the way out of the fields.
 *
 * With nothing changed there is nothing to write — but the user still asked to
 * be done editing, so go back to the preview instead of sitting on a dead
 * button.
 */
function onSaveClick() {
  const detail = detailRef.value
  if (!detail) return
  if (detail.dirty) detail.save()
  else previewing.value = true
}

/** Leave the fields without saving: back to reading, the draft dropped. */
function cancelEdit() {
  detailRef.value?.reset()
  previewing.value = true
}

function startEdit() {
  if (mode !== 'edit' || !item.value) return
  previewing.value = false
}

// ── Shortcuts (edit mode, reading) ──────────────────────────────────────────
// E = edit, F2 = rename a note's title — as in the capture window. ⌘/Ctrl+Enter
// is ItemDetail's own. Unlike the capture window, Esc deliberately does NOT
// cancel the fields here: a stray Esc must not discard a hand-written note, so
// dropping the draft takes the explicit Cancel button. Nothing
// fires mid-composition (IME) or while typing in a field.

function isTypingTarget(el: EventTarget | null): boolean {
  const node = el as HTMLElement | null
  if (!node) return false
  return node.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(node.tagName)
}

function onKeydown(e: KeyboardEvent) {
  if (e.isComposing || e.keyCode === 229) return
  if (mode !== 'edit' || !item.value || !previewing.value || confirmState.open) return
  if (isTypingTarget(e.target)) return
  if (e.key === 'F2') {
    e.preventDefault()
    detailRef.value?.startRename()
  } else if (e.key.toLowerCase() === 'e' && !e.metaKey && !e.ctrlKey && !e.altKey) {
    e.preventDefault()
    startEdit()
  }
}

onMounted(async () => {
  document.addEventListener('keydown', onKeydown)
  if (mode === 'edit') {
    await loadItem()
  } else {
    // Adopt the context this window was opened with (project / run being worked on).
    openCapture({
      tab: initialKind,
      context: { projectId: params.get('projectId'), runId: params.get('runId') },
    })

    // Reopening with a different tab focuses this window, switches here and
    // re-points the capture context. The draft itself is left alone.
    unlisten.push(
      await listen<ItemWindowSetTab>(ITEM_WINDOW_SET_TAB, (e) => {
        const payload = e.payload
        if (!payload) return
        if (payload.kind === 'todo' || payload.kind === 'note') tab.value = payload.kind
        setContext({ projectId: payload.projectId ?? null, runId: payload.runId ?? null })
      }),
      // A capture started elsewhere ("save this selection as a note"). It only
      // fills fields still empty here, so it can't clobber what's being typed.
      await listen<ItemPrefill>(ITEM_WINDOW_PREFILL, (e) => {
        const prefill = e.payload
        if (!prefill) return
        const adopted = adoptDraft({
          noteTitle: tab.value === 'note' ? prefill.title : undefined,
          noteContent: tab.value === 'note' ? prefill.content : undefined,
          todoText: tab.value === 'todo' ? prefill.content : undefined,
        })
        if (adopted.length) nextTick(() => formRef.value?.focus())
      }),
    )
  }

  // Only now is it safe for the opener to send a prefill (events aren't buffered).
  await emit(ITEM_WINDOW_READY, {})
})

onBeforeUnmount(() => {
  document.removeEventListener('keydown', onKeydown)
  unlisten.forEach((off) => off())
  unlisten = []
})
</script>

<template>
  <div class="flex h-screen w-screen flex-col bg-background text-foreground overflow-hidden">
    <!-- Slim titlebar — edit mode only, and only for what it alone can show:
         which item this is and whether it has unsaved changes. Every ACTION
         (done, delete, edit/save) lives in the action bar. Create mode has no
         titlebar at all: its Todo/Note tabs already say what the window is. -->
    <DetailWindowTitlebar
      v-if="mode === 'edit'"
      :icon="initialKind === 'note' ? StickyNote : ListTodo"
      :title="heading"
      :dirty="detailRef?.dirty"
      :pinned="pinned"
      :reloading="reloading"
      @toggle-pin="togglePin"
      @reload="reload"
    >
      <!-- A todo: its first sentence, struck through once done. -->
      <template v-if="initialKind === 'todo'" #label>
        <span
          class="truncate text-xs text-muted-foreground"
          :class="done && 'line-through decoration-muted-foreground/60'"
          :title="todoLine || heading"
        >{{ heading }}</span>
      </template>
    </DetailWindowTitlebar>

    <!-- Body -->
    <div class="min-h-0 flex-1 flex flex-col overflow-auto px-4 py-3">
      <!-- Create: the shared capture form, with its Todo / Note tabs -->
      <template v-if="mode === 'create'">
        <div class="mb-3 flex items-center gap-1">
          <button
            v-for="tabItem in TABS"
            :key="tabItem.key"
            type="button"
            class="inline-flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-xs font-medium transition-colors cursor-pointer"
            :class="tab === tabItem.key
              ? 'bg-accent text-foreground'
              : 'text-muted-foreground hover:text-foreground hover:bg-accent/50'"
            @click="tab = tabItem.key"
          >
            <component :is="tabItem.icon" class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t(tabItem.labelKey) }}
          </button>

          <!-- Pin: keep the capture window above other apps even when unfocused. -->
          <WindowPinButton class="ml-auto" :pinned="pinned" @toggle="togglePin" />
        </div>

        <QuickCaptureForm ref="formRef" @saved="onSaved" />
      </template>

      <!-- Edit: the shared item fields / read view -->
      <template v-else>
        <div v-if="loading" class="space-y-2">
          <div class="h-8 animate-pulse rounded-md border border-border bg-card" />
          <div class="h-48 animate-pulse rounded-md border border-border bg-card" />
        </div>

        <p v-else-if="missing" class="p-4 text-sm text-muted-foreground">
          {{ t('item.missing') }}
        </p>

        <!-- Esc must not discard work here: this window has no "cancel" to fall
             back to, and its close button is the OS's. -->
        <ItemDetail
          v-else
          ref="detailRef"
          :kind="initialKind"
          :item="item"
          :editing="!previewing"
          :show-run-link="false"
          :esc-cancels="false"
          @saved="onSaved"
          @changed="announce"
        />
      </template>
    </div>

    <!-- Actions (shared with the capture window). Create: the capture form's
         submit. Edit — reading: project on the left; Mark done (todo) / Copy
         markdown (note) · Edit · ⋯ (Copy ID, Delete) on the right. Writing:
         Cancel · Save (the form has its own project field). Saving is what returns to reading. -->
    <DetailWindowFooter
      v-if="!missing"
      :editing="!previewing"
      :can-save="!!detailRef?.canSave"
      :save-label="saveLabel"
      :copied-id="copiedId"
      @edit="startEdit"
      @cancel="cancelEdit"
      @save="onSaveClick"
      @copy-id="copyId"
      @delete="remove"
    >
      <template v-if="mode === 'create'" #default>
        <span class="mr-auto text-[11px] text-muted-foreground/70">⌘/Ctrl + Enter</span>
        <Button
          variant="primary"
          size="sm"
          :disabled="!formRef?.canSubmit || formRef?.saving"
          @click="formRef?.submit()"
        >
          {{ saveLabel }}
        </Button>
      </template>
      <template v-if="item" #left>
        <ProjectSelect :model-value="item.project_id" @update:model-value="onProjectChange" />
      </template>
      <template v-if="item" #actions>
        <Button v-if="initialKind === 'todo'" variant="outline" size="sm" @click="toggleDone">
          <component :is="done ? Undo2 : Check" class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ done ? t('item.markNotDone') : t('item.markDone') }}
        </Button>
        <!-- The body only, as the capture window copies a capture's. -->
        <CopyMarkdownButton v-else :text="(item as Note).content" />
      </template>
    </DetailWindowFooter>

    <!-- The pop-out doesn't mount the main app's dialog/toast hosts. -->
    <ConfirmModal />
    <ToastHost />
  </div>
</template>
