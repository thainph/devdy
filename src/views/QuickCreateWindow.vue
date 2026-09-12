<script setup lang="ts">
// Bare, chrome-less host for the standalone "quick create" window (opened via
// openQuickCreateWindow / `?quickCreateWindow=1`). Unlike the permission window
// this is a self-contained WRITER: it adds Todos/Notes straight to the shared
// SQLite DB via the Pinia stores (Tauri commands), so no cross-window state
// sync is needed. The main window picks up new rows on its next fetch.
//
// The fields themselves come from QuickCaptureForm, shared with the in-app
// slide-over (QuickCapturePanel) so both surfaces behave identically.
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { emit, listen, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { ListTodo, StickyNote, Pin, PinOff } from 'lucide-vue-next'
import { Button, ToastHost } from '@/components/ui'
import QuickCaptureForm from '@/components/QuickCaptureForm.vue'
import { useQuickCapture } from '@/composables/useQuickCapture'
import type { QuickCreateTab } from '@/lib/quickCreateWindow'

const { t } = useI18n()
// This is its own webview, so it gets its own copy of the capture state.
const { tab, openCapture, setContext } = useQuickCapture()

const params = new URLSearchParams(window.location.search)
const initialTab = (params.get('tab') as QuickCreateTab) || 'todo'

const pinned = ref(true)
const formRef = ref<InstanceType<typeof QuickCaptureForm> | null>(null)

let setTabUnlisten: UnlistenFn | null = null

const saveLabel = computed(() =>
  tab.value === 'todo' ? t('todos.quick.addTodo') : t('todos.quick.saveNote'),
)

function onSaved() {
  // Let the main window know so an open Todos/Notes screen can refresh live.
  emit('quickcreate:added', { tab: tab.value })
}

async function togglePin() {
  pinned.value = !pinned.value
  try {
    await getCurrentWindow().setAlwaysOnTop(pinned.value)
  } catch {
    /* window api unavailable */
  }
}

onMounted(async () => {
  document.title = t('todos.quick.windowTitle')
  // Adopt the context this window was opened with (project / run being worked on).
  openCapture({
    tab: initialTab === 'note' ? 'note' : 'todo',
    context: { projectId: params.get('projectId'), runId: params.get('runId') },
  })
  // Reopening the window with a different tab focuses it, switches here and
  // re-points the capture context at whatever the user is now working on. The
  // draft itself is left alone.
  setTabUnlisten = await listen<{
    tab: QuickCreateTab
    projectId?: string | null
    runId?: string | null
  }>('quickcreate:set-tab', (e) => {
    const payload = e.payload
    if (!payload) return
    if (payload.tab === 'todo' || payload.tab === 'note') tab.value = payload.tab
    setContext({ projectId: payload.projectId ?? null, runId: payload.runId ?? null })
  })
})

onBeforeUnmount(() => {
  setTabUnlisten?.()
  setTabUnlisten = null
})

const tabs: { key: QuickCreateTab; labelKey: string; icon: typeof ListTodo }[] = [
  { key: 'todo', labelKey: 'todos.quick.tabTodo', icon: ListTodo },
  { key: 'note', labelKey: 'todos.quick.tabNote', icon: StickyNote },
]
</script>

<template>
  <div class="flex h-screen w-screen flex-col bg-background text-foreground overflow-hidden">
    <!-- Slim titlebar with pin toggle -->
    <div class="flex items-center gap-2 px-3 h-9 border-b border-border/60 shrink-0">
      <span class="text-xs font-medium text-foreground/70">{{ t('todos.quick.heading') }}</span>
      <button
        type="button"
        class="ml-auto flex items-center gap-1 rounded px-1.5 py-1 text-[11px] text-foreground/60 hover:bg-accent/60 hover:text-foreground transition-colors cursor-pointer"
        :title="pinned ? t('todos.quick.pinnedTitle') : t('todos.quick.pinTitle')"
        @click="togglePin"
      >
        <component :is="pinned ? Pin : PinOff" class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ pinned ? t('todos.quick.pinned') : t('todos.quick.pin') }}
      </button>
    </div>

    <div class="flex-1 min-h-0 overflow-auto p-3">
      <!-- Tab switch -->
      <div class="flex items-center gap-1 mb-3">
        <button
          v-for="tabItem in tabs"
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
    </div>

    <!-- Actions -->
    <div class="flex items-center gap-2 border-t border-border/60 px-3 py-2.5 shrink-0">
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

    <!-- Local toast host (the pop-out doesn't mount the main app's ToastHost). -->
    <ToastHost />
  </div>
</template>
