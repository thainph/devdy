<script setup lang="ts">
// Bare, chrome-less host for the standalone "quick create" window (opened via
// openQuickCreateWindow / `?quickCreateWindow=1`). Unlike the permission window
// this is a self-contained WRITER: it adds Todos/Notes straight to the shared
// SQLite DB via the Pinia stores (Tauri commands), so no cross-window state
// sync is needed. The main window picks up new rows on its next fetch.
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { emit, listen, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { ListTodo, StickyNote, Pin, PinOff } from 'lucide-vue-next'
import { Button, Input, Textarea, AppSelect, ToastHost } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { useTodosStore } from '@/stores/todos'
import { useNotesStore } from '@/stores/notes'
import { useProjectsStore } from '@/stores/projects'
import type { QuickCreateTab } from '@/lib/quickCreateWindow'

const { t } = useI18n()
const { toast } = useToast()
const todos = useTodosStore()
const notes = useNotesStore()
const projects = useProjectsStore()

const initialTab = (new URLSearchParams(window.location.search).get('tab') as QuickCreateTab) || 'todo'
const tab = ref<QuickCreateTab>(initialTab === 'note' ? 'note' : 'todo')

const todoText = ref('')
const noteTitle = ref('')
const noteContent = ref('')
const noteProjectId = ref('')

const formRoot = ref<HTMLElement | null>(null)
const saving = ref(false)
const pinned = ref(true)

let setTabUnlisten: UnlistenFn | null = null

const projectOptions = computed(() => [
  { value: '', label: t('todos.quick.noProject') },
  ...projects.projects.map((p) => ({ value: p.id, label: p.name })),
])

function focusActiveField() {
  nextTick(() => {
    const selector = tab.value === 'todo' ? 'textarea' : 'input'
    formRoot.value?.querySelector<HTMLElement>(selector)?.focus()
  })
}

const canSubmit = computed(() =>
  tab.value === 'todo'
    ? todoText.value.trim().length > 0
    : noteTitle.value.trim().length > 0 || noteContent.value.trim().length > 0,
)

async function submit() {
  if (!canSubmit.value || saving.value) return
  saving.value = true
  try {
    if (tab.value === 'todo') {
      await todos.add(todoText.value)
      todoText.value = ''
      toast.success(t('todos.quick.todoAdded'))
    } else {
      await notes.add(noteTitle.value, noteContent.value, noteProjectId.value || null)
      noteTitle.value = ''
      noteContent.value = ''
      toast.success(t('todos.quick.noteSaved'))
    }
    // Let the main window know so an open Todos/Notes screen can refresh live.
    emit('quickcreate:added', { tab: tab.value })
    focusActiveField()
  } catch (e) {
    toast.error(String(e))
  } finally {
    saving.value = false
  }
}

function onKeydown(e: KeyboardEvent) {
  if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
    e.preventDefault()
    submit()
  }
}

async function togglePin() {
  pinned.value = !pinned.value
  try {
    await getCurrentWindow().setAlwaysOnTop(pinned.value)
  } catch {
    /* window api unavailable */
  }
}

watch(tab, focusActiveField)

onMounted(async () => {
  document.title = t('todos.quick.windowTitle')
  projects.fetchProjects()
  focusActiveField()
  // Reopening the window with a different tab focuses it and switches here.
  setTabUnlisten = await listen<{ tab: QuickCreateTab }>('quickcreate:set-tab', (e) => {
    if (e.payload?.tab === 'todo' || e.payload?.tab === 'note') tab.value = e.payload.tab
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

    <div ref="formRoot" class="flex-1 min-h-0 overflow-auto p-3" @keydown="onKeydown">
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

      <!-- Todo form -->
      <div v-if="tab === 'todo'" class="space-y-1.5">
        <Textarea v-model="todoText" rows="4" :placeholder="t('todos.quick.todoPlaceholder')" />
      </div>

      <!-- Note form -->
      <div v-else class="space-y-2.5">
        <Input v-model="noteTitle" :placeholder="t('todos.quick.noteTitlePlaceholder')" />
        <Textarea v-model="noteContent" rows="5" :placeholder="t('todos.quick.notePlaceholder')" />
        <AppSelect v-model="noteProjectId" :options="projectOptions" :placeholder="t('todos.quick.noProject')" />
      </div>
    </div>

    <!-- Actions -->
    <div class="flex items-center gap-2 border-t border-border/60 px-3 py-2.5 shrink-0">
      <span class="mr-auto text-[11px] text-muted-foreground/70">⌘/Ctrl + Enter</span>
      <Button variant="primary" size="sm" :disabled="!canSubmit || saving" @click="submit">
        {{ tab === 'todo' ? t('todos.quick.addTodo') : t('todos.quick.saveNote') }}
      </Button>
    </div>

    <!-- Local toast host (the pop-out doesn't mount the main app's ToastHost). -->
    <ToastHost />
  </div>
</template>
