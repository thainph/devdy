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
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { emit, listen, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { Check, Copy, ListTodo, Pencil, StickyNote, Trash2, Undo2 } from 'lucide-vue-next'
import { Button, ConfirmModal, ToastHost } from '@/components/ui'
import QuickCaptureForm from '@/components/QuickCaptureForm.vue'
import ItemDetail from '@/components/ItemDetail.vue'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { useQuickCapture } from '@/composables/useQuickCapture'
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

const { t } = useI18n()
const { toast } = useToast()
const { confirm } = useConfirm()
const notes = useNotesStore()
const todos = useTodosStore()
// This is its own webview, so it gets its own copy of the capture state.
const { tab, openCapture, setContext, adoptDraft } = useQuickCapture()

const params = new URLSearchParams(window.location.search)
const mode: ItemWindowMode = params.get('mode') === 'edit' ? 'edit' : 'create'
const initialKind: ItemKind = params.get('kind') === 'note' ? 'note' : 'todo'
const itemId = params.get('id') ?? ''

const loading = ref(mode === 'edit')
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
// A todo has no title, so the heading showed a constant ("Edit task") that said
// nothing. Its id is the one thing here that identifies it — and the thing the
// AI side needs to address it — so show that instead, next to Copy ID.
const heading = computed(() => {
  if (initialKind === 'todo') return itemId || t('item.editTodoHeading')
  return (item.value as Note | null)?.title?.trim() || t('item.editNoteHeading')
})
/** The id is an identifier, not prose: monospaced, and never struck through. */
const headingIsId = computed(() => initialKind === 'todo' && !!itemId)

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

onMounted(async () => {
  document.title = mode === 'create' ? t('item.createWindowTitle') : t('item.editWindowTitle')

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
    <div v-if="mode === 'edit'" class="flex items-center gap-2 px-3 h-9 border-b border-border/60 shrink-0">
      <component
        :is="initialKind === 'note' ? StickyNote : ListTodo"
        class="h-3.5 w-3.5 shrink-0 text-muted-foreground"
        :stroke-width="1.75"
      />
      <span
        class="truncate text-xs text-foreground/70"
        :class="headingIsId
          ? 'font-mono text-[11px] text-muted-foreground'
          : done ? 'font-medium line-through decoration-muted-foreground/60' : 'font-medium'"
        :title="heading"
      >{{ heading }}</span>
      <span v-if="detailRef?.dirty" class="shrink-0 text-[11px] text-muted-foreground/70">
        · {{ t('item.unsaved') }}
      </span>

    </div>

    <!-- Body -->
    <div class="min-h-0 flex-1 overflow-auto p-3">
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
        />
      </template>
    </div>

    <!-- Actions. In edit mode the two modes share one slot: reading offers
         Edit, writing offers Save — and saving is what returns to reading, so
         the window never shows both at once. -->
    <div v-if="!missing" class="flex items-center gap-2 border-t border-border/60 px-3 py-2.5 shrink-0">
      <Button v-if="mode === 'edit'" variant="destructive" size="sm" @click="remove">
        <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ t('common.delete') }}
      </Button>
      <Button
        v-if="mode === 'edit' && initialKind === 'todo'"
        variant="outline"
        size="sm"
        @click="toggleDone"
      >
        <component :is="done ? Undo2 : Check" class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ done ? t('item.markNotDone') : t('item.markDone') }}
      </Button>
      <Button
        v-if="mode === 'edit' && item"
        variant="outline"
        size="sm"
        @click="copyId"
      >
        <!-- Label stays put: the tick and the toast carry the feedback, and a
             swapping label would resize the button mid-row. -->
        <component :is="copiedId ? Check : Copy" class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ t('item.copyId') }}
      </Button>
      <span v-if="!previewing" class="mr-auto text-[11px] text-muted-foreground/70">⌘/Ctrl + Enter</span>
      <span v-else class="mr-auto" />

      <Button
        v-if="mode === 'create'"
        variant="primary"
        size="sm"
        :disabled="!formRef?.canSubmit || formRef?.saving"
        @click="formRef?.submit()"
      >
        {{ saveLabel }}
      </Button>
      <Button v-else-if="previewing" size="sm" @click="previewing = false">
        <Pencil class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ t('item.edit') }}
      </Button>
      <Button v-else variant="primary" size="sm" :disabled="!detailRef?.canSave" @click="onSaveClick">
        {{ saveLabel }}
      </Button>
    </div>

    <!-- The pop-out doesn't mount the main app's dialog/toast hosts. -->
    <ConfirmModal />
    <ToastHost />
  </div>
</template>
