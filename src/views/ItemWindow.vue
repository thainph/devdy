<script setup lang="ts">
// THE capture window — "New todo / note" (opened via lib/itemWindow,
// `?itemWindow=1&mode=create&…`): QuickCaptureForm with Todo / Note tabs; it
// stays open so several thoughts can be captured in a row.
//
// Reading and editing an existing item happen in the Notes / Todos windows
// (lib/itemListWindow). This window saves straight to the shared SQLite DB
// through the Pinia stores, then broadcasts ITEM_CHANGED so those windows (other
// webviews, with their own store copies) refetch.
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { emit, listen, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { ListTodo, StickyNote } from 'lucide-vue-next'
import { Button, ConfirmModal, ToastHost } from '@/components/ui'
import WindowPinButton from '@/components/WindowPinButton.vue'
import QuickCaptureForm from '@/components/QuickCaptureForm.vue'
import { useQuickCapture } from '@/composables/useQuickCapture'
import { useFloatingWindow } from '@/composables/useFloatingWindow'
import {
  ITEM_CHANGED,
  ITEM_WINDOW_PREFILL,
  ITEM_WINDOW_READY,
  ITEM_WINDOW_SET_TAB,
  type ItemKind,
  type ItemPrefill,
  type ItemWindowSetTab,
} from '@/lib/itemWindow'
import { refreshTray } from '@/lib/tray'

const { t } = useI18n()
// This is its own webview, so it gets its own copy of the capture state.
const { tab, openCapture, setContext, adoptDraft } = useQuickCapture()
// Floats above other apps while focused; pin to keep it on top deliberately.
const { pinned, togglePin } = useFloatingWindow()

const params = new URLSearchParams(window.location.search)
const initialKind: ItemKind = params.get('kind') === 'note' ? 'note' : 'todo'

const formRef = ref<InstanceType<typeof QuickCaptureForm> | null>(null)

let unlisten: UnlistenFn[] = []

// The OS window title doubles as this window's row label in the menu-bar
// switcher (lib/tray.ts); it follows the active tab.
const windowTitle = computed(() => (tab.value === 'note' ? t('item.newNote') : t('item.newTodo')))
watch(
  windowTitle,
  async (title) => {
    document.title = title
    // Setting document.title alone does NOT update the OS window title in Tauri,
    // and the menu-bar switcher reads the OS title (lib/tray.ts).
    try {
      await getCurrentWindow().setTitle(title)
    } catch {
      /* outside the Tauri shell */
    }
    void refreshTray()
  },
  { immediate: true },
)

const saveLabel = computed(() => (tab.value === 'todo' ? t('item.addTodo') : t('item.saveNote')))

const TABS: { key: ItemKind; labelKey: string; icon: typeof ListTodo }[] = [
  { key: 'todo', labelKey: 'item.tabTodo', icon: ListTodo },
  { key: 'note', labelKey: 'item.tabNote', icon: StickyNote },
]

function onSaved() {
  emit(ITEM_CHANGED, { kind: tab.value })
}

onMounted(async () => {
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

  // Only now is it safe for the opener to send a prefill (events aren't buffered).
  await emit(ITEM_WINDOW_READY, {})
})

onBeforeUnmount(() => {
  unlisten.forEach((off) => off())
  unlisten = []
})
</script>

<template>
  <div class="flex h-screen w-screen flex-col overflow-hidden bg-background text-foreground">
    <div class="flex min-h-0 flex-1 flex-col overflow-auto px-4 py-3">
      <div class="mb-3 flex items-center gap-1">
        <button
          v-for="tabItem in TABS"
          :key="tabItem.key"
          type="button"
          class="inline-flex cursor-pointer items-center gap-1.5 rounded-md px-2.5 py-1.5 text-xs font-medium transition-colors"
          :class="tab === tabItem.key
            ? 'bg-accent text-foreground'
            : 'text-muted-foreground hover:bg-accent/50 hover:text-foreground'"
          @click="tab = tabItem.key"
        >
          <component :is="tabItem.icon" class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t(tabItem.labelKey) }}
        </button>

        <!-- Pin: keep the capture window above other apps even when unfocused. -->
        <WindowPinButton class="ml-auto" :pinned="pinned" @toggle="togglePin" />
      </div>

      <QuickCaptureForm ref="formRef" @saved="onSaved" />
    </div>

    <!-- Submit bar: the shortcut on the left, the capture form's submit on the right. -->
    <div class="flex shrink-0 items-center gap-2 border-t border-border/60 px-3 py-2.5">
      <span class="mr-auto text-[11px] text-muted-foreground/70">⌘/Ctrl + Enter</span>
      <Button
        variant="primary"
        size="sm"
        :disabled="!formRef?.canSubmit || formRef?.saving"
        @click="formRef?.submit()"
      >
        {{ saveLabel }}
      </Button>
    </div>

    <!-- The pop-out doesn't mount the main app's dialog/toast hosts. -->
    <ConfirmModal />
    <ToastHost />
  </div>
</template>
