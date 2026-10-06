<script setup lang="ts">
// THE todo / note LIST — a standalone, chrome-less OS window that sits BESIDE the
// main window (opened via lib/itemListWindow, `?itemListWindow=1&kind=…`).
//
// It replaced the right-side drawer, which covered the screen underneath. A real
// window sits next to the main one instead: the opener splits the desktop 2/3
// (main) + 1/3 (list), so reading a todo never hides what the app was showing.
//
// It only READS. Adding and editing happen in the item window (lib/itemWindow),
// so there is exactly one place these objects are written, and no half-typed state
// to lose when this closes. Todos are one-line checklist rows (scanning is the
// job); notes are cards with a short preview, since their body is the point.
//
// Its own webview means its own store copy, so an ITEM_CHANGED broadcast from the
// item window is what triggers the refetch.
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { emit, listen, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { Check, Folder, ListTodo, MessageSquare, Plus, Search, StickyNote, Trash2, X } from 'lucide-vue-next'
import { AppSelect, Button, ConfirmModal, Input, ToastHost } from '@/components/ui'
import { useConfirm } from '@/composables/useConfirm'
import { useProjectsStore } from '@/stores/projects'
import { useNotesStore } from '@/stores/notes'
import { useTodosStore } from '@/stores/todos'
import {
  ITEM_CHANGED,
  openItemCreateWindow,
  openItemEditWindow,
  type ItemKind,
} from '@/lib/itemWindow'
import {
  ITEM_LIST_OPEN_RUN,
  ITEM_LIST_SET_KIND,
  type ItemListOpenRun,
  type ItemListSetKind,
} from '@/lib/itemListWindow'
import { refreshTray } from '@/lib/tray'

const { t } = useI18n()
const { confirm } = useConfirm()
const todos = useTodosStore()
const notes = useNotesStore()
const projects = useProjectsStore()

const params = new URLSearchParams(window.location.search)
const kind = ref<ItemKind>(params.get('kind') === 'note' ? 'note' : 'todo')
/** The project being worked on, used to offer "this project only". */
const contextProjectId = ref<string | null>(params.get('projectId') || null)

const search = ref('')
/** Project filter: '' = all projects, NO_PROJECT = items without one, else a project id. */
const NO_PROJECT = '__none__'
// Default to the project the window was opened from, so "its" items show first.
const projectFilter = ref<string>(contextProjectId.value ?? '')
/** Todo status filter: 'active' (not done, the default) | 'done' | 'all'. */
const statusFilter = ref<string>('active')

let unlisten: UnlistenFn[] = []

const TABS: { key: ItemKind; labelKey: string; icon: typeof ListTodo }[] = [
  { key: 'todo', labelKey: 'item.tabTodo', icon: ListTodo },
  { key: 'note', labelKey: 'item.tabNote', icon: StickyNote },
]

/** One shape for both kinds, so the row template stays single-purpose. */
interface Row {
  id: string
  title: string
  body: string
  /** Notes only: the body as plain prose for the card's two-line preview. */
  preview: string
  done: boolean
  projectId: string | null
  /** The AI session it was captured from, for the backlink. */
  runId: string | null
  createdAt: string
}

const rows = computed<Row[]>(() =>
  kind.value === 'todo'
    ? todos.todos.map((todo) => ({
        id: todo.id,
        title: firstLine(todo.text),
        body: todo.text,
        preview: '',
        done: todo.done,
        projectId: todo.project_id,
        runId: todo.run_id,
        createdAt: todo.created_at,
      }))
    : notes.notes.map((note) => {
        const hasTitle = !!note.title.trim()
        return {
          id: note.id,
          title: hasTitle ? note.title.trim() : firstLine(note.content) || t('item.untitled'),
          body: note.content,
          preview: noteSnippet(note.content, hasTitle),
          done: false,
          projectId: note.project_id,
          runId: note.run_id,
          createdAt: note.created_at,
        }
      }),
)

const filtered = computed(() => {
  const q = search.value.trim().toLowerCase()
  return rows.value.filter((row) => {
    if (kind.value === 'todo') {
      if (statusFilter.value === 'active' && row.done) return false
      if (statusFilter.value === 'done' && !row.done) return false
    }
    if (projectFilter.value === NO_PROJECT) {
      if (row.projectId) return false
    } else if (projectFilter.value && row.projectId !== projectFilter.value) {
      return false
    }
    if (q && !`${row.title}\n${row.body}`.toLowerCase().includes(q)) return false
    return true
  })
})

/**
 * The project dropdown's options, built from the projects actually used by the
 * current tab's items (plus the currently-selected one, so a filter never points
 * at a missing option), with "all" first and "no project" last when relevant.
 */
const projectOptions = computed(() => {
  const ids = new Set<string>()
  let hasNone = false
  for (const row of rows.value) {
    if (row.projectId) ids.add(row.projectId)
    else hasNone = true
  }
  if (projectFilter.value && projectFilter.value !== NO_PROJECT) ids.add(projectFilter.value)
  const named = [...ids]
    .map((id) => ({ value: id, label: projectName(id) ?? id }))
    .sort((a, b) => a.label.localeCompare(b.label))
  const opts = [{ value: '', label: t('item.allProjects') }, ...named]
  if (hasNone) opts.push({ value: NO_PROJECT, label: t('item.noProject') })
  return opts
})

const statusOptions = computed(() => [
  { value: 'active', label: t('item.statusActive') },
  { value: 'done', label: t('item.statusDone') },
  { value: 'all', label: t('item.statusAll') },
])

const hasActiveFilters = computed(
  () =>
    !!search.value ||
    !!projectFilter.value ||
    (kind.value === 'todo' && statusFilter.value !== 'active'),
)

/** A todo row names its project only while the list mixes projects. */
const showTodoProject = computed(() => projectFilter.value === '')

function resetFilters() {
  search.value = ''
  projectFilter.value = ''
  statusFilter.value = 'active'
}

function projectName(id: string | null): string | null {
  if (!id) return null
  return projects.projects.find((p) => p.id === id)?.name ?? null
}

/** A row's one-line label: the first non-empty line, stripped of markdown noise. */
function firstLine(text: string): string {
  const line = text.split('\n').find((l) => l.trim()) ?? ''
  return line
    .replace(/^\s*[-*+]\s+\[[ xX]\]\s*/, '') // checklist marker
    .replace(/^\s*[-*+]\s+/, '') // bullet
    .replace(/^\s*#{1,6}\s+/, '') // heading
    .replace(/^\s*>\s?/, '') // quote
    .trim()
}

/**
 * A note's body flattened to plain prose for the card preview. When the title was
 * derived from the body's first line, that line is dropped so it isn't repeated.
 */
function noteSnippet(content: string, hasTitle: boolean): string {
  const lines = content.split('\n').map(firstLine).filter(Boolean)
  return (hasTitle ? lines : lines.slice(1)).join(' ')
}

/** Compact "2h", "3d" style age of an item, falling back to a short date. */
function relativeTime(iso: string): string {
  const diff = Date.now() - new Date(iso).getTime()
  const mins = Math.round(diff / 60000)
  if (mins < 1) return t('item.justNow')
  if (mins < 60) return t('item.minsAgo', { mins })
  const hours = Math.round(mins / 60)
  if (hours < 24) return t('item.hoursAgo', { hours })
  const days = Math.round(hours / 24)
  if (days < 30) return t('item.daysAgo', { days })
  return new Date(iso).toLocaleDateString(undefined, { month: 'short', day: 'numeric' })
}

/** Pull the current rows from the DB (the item window may have changed them). */
function refresh() {
  if (kind.value === 'todo') todos.fetchTodos()
  else notes.fetchNotes()
}

function openCreate() {
  openItemCreateWindow(kind.value, { projectId: contextProjectId.value })
}

function openItem(id: string) {
  openItemEditWindow(kind.value, id)
}

/**
 * Jump to the AI session this item was captured from.
 *
 * This window has no router of the main app, so ask the main window to focus
 * itself and navigate there (see App.vue's ITEM_LIST_OPEN_RUN listener).
 */
function openLinkedRun(row: Row) {
  if (!row.projectId || !row.runId) return
  emit(ITEM_LIST_OPEN_RUN, { projectId: row.projectId, runId: row.runId } satisfies ItemListOpenRun)
}

async function toggleDone(id: string) {
  if (kind.value !== 'todo') return
  await todos.toggle(id)
}

async function remove(row: Row) {
  const isNote = kind.value === 'note'
  if (
    !(await confirm({
      title: isNote ? t('item.confirm.deleteNoteTitle') : t('item.confirm.deleteTodoTitle'),
      message: isNote ? t('item.confirm.deleteNoteMessage') : t('item.confirm.deleteTodoMessage'),
      confirmLabel: t('common.delete'),
    }))
  ) {
    return
  }
  if (isNote) await notes.remove(row.id)
  else await todos.remove(row.id)
}

// The OS window title doubles as this window's row label in the menu-bar switcher
// (lib/tray.ts); keep it on the active tab so the titlebar names what's shown.
const windowTitle = computed(() =>
  kind.value === 'note' ? t('item.navNotes') : t('item.navTodos'),
)
watch(
  windowTitle,
  async (title) => {
    document.title = title
    try {
      await getCurrentWindow().setTitle(title)
    } catch {
      /* outside the Tauri shell */
    }
    void refreshTray()
  },
  { immediate: true },
)

// Switching tab shows a different list; clear the search and resync.
watch(kind, () => {
  search.value = ''
  refresh()
})

// Reopened from a different project context → refocus the filter on that project.
watch(contextProjectId, (id) => {
  projectFilter.value = id ?? ''
})

onMounted(async () => {
  refresh()
  projects.fetchProjects()
  try {
    unlisten.push(
      // A write in the item window (a different webview) → refetch our rows.
      await listen<{ kind: string }>(ITEM_CHANGED, (e) => {
        if (!e.payload?.kind || e.payload.kind === kind.value) refresh()
      }),
      // Reopened from the main window with a different tab / project context.
      await listen<ItemListSetKind>(ITEM_LIST_SET_KIND, (e) => {
        const payload = e.payload
        if (!payload) return
        if (payload.kind === 'todo' || payload.kind === 'note') kind.value = payload.kind
        if (payload.projectId !== undefined) contextProjectId.value = payload.projectId ?? null
      }),
    )
  } catch {
    /* running outside the Tauri shell */
  }
})

onBeforeUnmount(() => {
  unlisten.forEach((off) => off())
  unlisten = []
})
</script>

<template>
  <div class="flex h-screen w-screen flex-col bg-background text-foreground overflow-hidden">
    <!-- Header: Todo / Note tabs + New, mirroring the old drawer header. -->
    <div class="flex items-center gap-2 border-b border-border/60 px-3 h-11 shrink-0">
      <div class="flex min-w-0 flex-1 items-center gap-1">
        <button
          v-for="item in TABS"
          :key="item.key"
          type="button"
          class="inline-flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-xs font-medium transition-colors cursor-pointer"
          :class="kind === item.key
            ? 'bg-accent text-foreground'
            : 'text-muted-foreground hover:text-foreground hover:bg-accent/50'"
          @click="kind = item.key"
        >
          <component :is="item.icon" class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t(item.labelKey) }}
        </button>
        <span class="ml-1 text-[11px] tabular-nums text-muted-foreground/60">{{ filtered.length }}</span>
      </div>
      <Button size="sm" @click="openCreate">
        <Plus class="h-3.5 w-3.5" :stroke-width="2" />
        {{ kind === 'todo' ? t('item.newTodo') : t('item.newNote') }}
      </Button>
    </div>

    <div class="flex min-h-0 flex-1 flex-col">
      <!-- Search + filters -->
      <div class="shrink-0 space-y-2 border-b border-border/60 px-4 py-3">
        <div class="relative">
          <Search
            class="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground"
            :stroke-width="1.75"
          />
          <Input v-model="search" class="pl-8" :placeholder="t('item.searchPlaceholder')" />
          <button
            v-if="search"
            type="button"
            class="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground cursor-pointer"
            :title="t('item.clearSearch')"
            @click="search = ''"
          >
            <X class="h-3.5 w-3.5" :stroke-width="2" />
          </button>
        </div>

        <div class="flex items-center gap-1.5">
          <div class="min-w-0 flex-1">
            <AppSelect v-model="projectFilter" :options="projectOptions" size="sm">
              <template #leading>
                <Folder class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.75" />
              </template>
            </AppSelect>
          </div>
          <div v-if="kind === 'todo'" class="w-28 shrink-0">
            <AppSelect v-model="statusFilter" :options="statusOptions" size="sm" />
          </div>
          <button
            v-if="hasActiveFilters"
            type="button"
            class="shrink-0 cursor-pointer rounded-md p-1.5 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
            :title="t('item.resetFilters')"
            @click="resetFilters"
          >
            <X class="h-3.5 w-3.5" :stroke-width="2" />
          </button>
        </div>
      </div>

      <!-- Rows: todo checklist / note cards; opening one is the item window's job -->
      <div class="min-h-0 flex-1 overflow-auto p-2">
        <div
          v-if="rows.length === 0"
          class="flex flex-col items-center justify-center px-4 py-10 text-center"
        >
          <div class="mb-3 flex h-10 w-10 items-center justify-center rounded-xl bg-muted">
            <component
              :is="kind === 'todo' ? ListTodo : StickyNote"
              class="h-5 w-5 text-muted-foreground"
              :stroke-width="1.5"
            />
          </div>
          <p class="text-xs text-muted-foreground">
            {{ kind === 'todo' ? t('item.emptyTodo') : t('item.emptyNote') }}
          </p>
          <Button size="sm" class="mt-3" @click="openCreate">
            <Plus class="h-3.5 w-3.5" :stroke-width="2" />
            {{ kind === 'todo' ? t('item.newTodo') : t('item.newNote') }}
          </Button>
        </div>

        <p
          v-else-if="filtered.length === 0"
          class="px-3 py-8 text-center text-[11px] text-muted-foreground"
        >
          {{ t('item.noResults') }}
        </p>

        <ul v-else :class="kind === 'todo' ? 'space-y-px' : 'space-y-2'">
          <li v-for="row in filtered" :key="row.id" class="group relative">
            <!-- Todo: a compact one-line checklist row. -->
            <div
              v-if="kind === 'todo'"
              class="flex items-center gap-2.5 rounded-md px-2 py-1.5 transition-colors hover:bg-accent/50"
            >
              <button
                type="button"
                class="flex h-4 w-4 shrink-0 cursor-pointer items-center justify-center rounded-[5px] border transition-colors"
                :class="row.done
                  ? 'bg-primary border-primary text-primary-foreground'
                  : 'border-muted-foreground/40 hover:border-primary'"
                :title="row.done ? t('item.markNotDone') : t('item.markDone')"
                @click="toggleDone(row.id)"
              >
                <Check v-if="row.done" class="h-2.5 w-2.5" :stroke-width="3" />
              </button>
              <button
                type="button"
                class="min-w-0 flex-1 cursor-pointer truncate text-left text-[13px] leading-snug"
                :class="row.done ? 'text-muted-foreground line-through' : 'text-foreground'"
                :title="t('item.openTitle')"
                @click="openItem(row.id)"
              >
                {{ row.title || t('item.untitled') }}
              </button>
              <span
                v-if="showTodoProject && projectName(row.projectId)"
                class="max-w-[35%] shrink-0 truncate text-[11px] text-muted-foreground/70"
              >
                {{ projectName(row.projectId) }}
              </span>
            </div>

            <!-- Note: a card — title, a two-line preview, then project · age. -->
            <button
              v-else
              type="button"
              class="block w-full cursor-pointer rounded-lg border border-border/70 bg-card px-3 py-2.5 text-left transition-colors hover:border-border hover:bg-accent/40"
              :title="t('item.openTitle')"
              @click="openItem(row.id)"
            >
              <span class="block truncate text-[13px] font-medium leading-snug text-foreground">
                {{ row.title || t('item.untitled') }}
              </span>
              <span
                v-if="row.preview"
                class="mt-1 line-clamp-2 text-xs leading-relaxed text-muted-foreground"
              >
                {{ row.preview }}
              </span>
              <span class="mt-2 flex items-center gap-1.5 text-[10px] text-muted-foreground/70">
                <template v-if="projectName(row.projectId)">
                  <Folder class="h-3 w-3 shrink-0" :stroke-width="1.75" />
                  <span class="truncate">{{ projectName(row.projectId) }}</span>
                  <span aria-hidden="true">·</span>
                </template>
                <span class="shrink-0 tabular-nums">{{ relativeTime(row.createdAt) }}</span>
              </span>
            </button>

            <!-- Row actions float over the right edge on hover, so they take no
                 space at rest and every row keeps the same width. -->
            <div
              class="pointer-events-none absolute flex items-center gap-0.5 rounded-md border border-border/60 bg-popover p-0.5 opacity-0 shadow-sm transition-opacity group-hover:pointer-events-auto group-hover:opacity-100 focus-within:pointer-events-auto focus-within:opacity-100"
              :class="kind === 'todo' ? 'right-1.5 top-1/2 -translate-y-1/2' : 'right-2 top-2'"
            >
              <button
                v-if="row.projectId && row.runId"
                type="button"
                class="cursor-pointer rounded p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-primary"
                :title="kind === 'note' ? t('item.openRunNoteTitle') : t('item.openRunTodoTitle')"
                @click="openLinkedRun(row)"
              >
                <MessageSquare class="h-3.5 w-3.5" :stroke-width="1.75" />
              </button>
              <button
                type="button"
                class="cursor-pointer rounded p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-destructive"
                :title="t('common.delete')"
                @click="remove(row)"
              >
                <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
              </button>
            </div>
          </li>
        </ul>
      </div>
    </div>

    <!-- The pop-out doesn't mount the main app's dialog/toast hosts. -->
    <ConfirmModal />
    <ToastHost />
  </div>
</template>
