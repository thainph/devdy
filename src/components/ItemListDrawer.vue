<script setup lang="ts">
// THE todo / note list — a right-side drawer mounted once in App.vue and
// reachable from every screen (⌘⇧K, the sidebar button, the View menu).
//
// This replaced the /todos and /notes screens outright. A list screen forced a
// route change, which remounts the run workspace and throws away stream scroll
// position, open panels and the viewed diff — a heavy price for "what was that
// task again?". The drawer leaves the screen underneath completely untouched.
//
// It only READS. Adding and editing happen in the standalone item window (see
// lib/itemWindow), so there is exactly one place where these objects are
// written, and no half-typed state to lose when this closes. Rows are one line
// each: scanning is the job here, and a wall of expanded markdown isn't
// scannable.
//
// Rows come from the shared Pinia stores; the window is a different webview with
// its own store copy, so its ITEM_CHANGED broadcast triggers the refetch.
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { Check, ListTodo, MessageSquare, Plus, Search, StickyNote, Trash2, X } from 'lucide-vue-next'
import { Button, Drawer, Input } from '@/components/ui'
import { useConfirm } from '@/composables/useConfirm'
import { useItemPanel } from '@/composables/useItemPanel'
import { useProjectsStore } from '@/stores/projects'
import { useNotesStore } from '@/stores/notes'
import { useTodosStore } from '@/stores/todos'
import {
  ITEM_CHANGED,
  openItemCreateWindow,
  openItemEditWindow,
  type ItemKind,
} from '@/lib/itemWindow'

const { t } = useI18n()
const router = useRouter()
const { confirm } = useConfirm()
const { open, kind, contextProjectId, closePanel } = useItemPanel()
const todos = useTodosStore()
const notes = useNotesStore()
const projects = useProjectsStore()

const search = ref('')
const onlyThisProject = ref(false)
const hideDone = ref(true)

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
  done: boolean
  projectId: string | null
  /** The AI session it was captured from, for the backlink. */
  runId: string | null
}

const rows = computed<Row[]>(() =>
  kind.value === 'todo'
    ? todos.todos.map((todo) => ({
        id: todo.id,
        title: firstLine(todo.text),
        body: todo.text,
        done: todo.done,
        projectId: todo.project_id,
        runId: todo.run_id,
      }))
    : notes.notes.map((note) => ({
        id: note.id,
        title: note.title.trim() || firstLine(note.content) || t('item.untitled'),
        body: note.content,
        done: false,
        projectId: note.project_id,
        runId: note.run_id,
      })),
)

const filtered = computed(() => {
  const q = search.value.trim().toLowerCase()
  return rows.value.filter((row) => {
    if (kind.value === 'todo' && hideDone.value && row.done) return false
    if (onlyThisProject.value && contextProjectId.value && row.projectId !== contextProjectId.value) {
      return false
    }
    if (q && !`${row.title}\n${row.body}`.toLowerCase().includes(q)) return false
    return true
  })
})

const doneCount = computed(() =>
  kind.value === 'todo' ? todos.todos.filter((x) => x.done).length : 0,
)

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
 * The one action here that IS a route change — the user asked to go to that
 * session — so the drawer closes with it.
 */
function openLinkedRun(row: Row) {
  if (!row.projectId || !row.runId) return
  closePanel()
  router
    .push({ name: 'project-run-detail', params: { projectId: row.projectId, runId: row.runId } })
    .catch(() => {})
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

// Switching tab shows a different list; clear the search and resync.
watch(kind, () => {
  search.value = ''
  refresh()
})

// Opening the drawer is the moment its rows must be right.
watch(open, (isOpen) => {
  if (!isOpen) return
  refresh()
  projects.fetchProjects()
})

onMounted(async () => {
  try {
    unlisten.push(
      await listen<{ kind: string }>(ITEM_CHANGED, (e) => {
        if (!e.payload?.kind || e.payload.kind === kind.value) refresh()
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
  <Drawer :open="open" side="right" size="lg" @close="closePanel">
    <template #header>
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
    </template>

    <div class="flex h-full flex-col">
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

        <div
          v-if="contextProjectId || (kind === 'todo' && doneCount > 0)"
          class="flex flex-wrap items-center gap-1.5"
        >
          <button
            v-if="contextProjectId"
            type="button"
            class="cursor-pointer rounded-full border px-2 py-0.5 text-[11px] transition-colors"
            :class="onlyThisProject
              ? 'border-primary/50 bg-primary/10 text-foreground'
              : 'border-border text-muted-foreground hover:text-foreground'"
            @click="onlyThisProject = !onlyThisProject"
          >
            {{ t('item.onlyThisProject') }}
          </button>
          <button
            v-if="kind === 'todo' && doneCount > 0"
            type="button"
            class="cursor-pointer rounded-full border px-2 py-0.5 text-[11px] transition-colors"
            :class="hideDone
              ? 'border-primary/50 bg-primary/10 text-foreground'
              : 'border-border text-muted-foreground hover:text-foreground'"
            @click="hideDone = !hideDone"
          >
            {{ t('item.hideDone', { count: doneCount }) }}
          </button>
        </div>
      </div>

      <!-- Rows: one line each; opening one is the item window's job -->
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

        <ul v-else class="space-y-0.5">
          <li
            v-for="row in filtered"
            :key="row.id"
            class="group flex items-center gap-1.5 rounded-md px-2 py-1.5 transition-colors hover:bg-accent/40"
          >
            <button
              v-if="kind === 'todo'"
              type="button"
              class="flex h-4 w-4 shrink-0 cursor-pointer items-center justify-center rounded border transition-colors"
              :class="row.done
                ? 'bg-primary border-primary text-primary-foreground'
                : 'border-border hover:border-primary/60'"
              :title="row.done ? t('item.markNotDone') : t('item.markDone')"
              @click="toggleDone(row.id)"
            >
              <Check v-if="row.done" class="h-2.5 w-2.5" :stroke-width="3" />
            </button>

            <button
              type="button"
              class="min-w-0 flex-1 cursor-pointer text-left"
              :title="t('item.openTitle')"
              @click="openItem(row.id)"
            >
              <span
                class="block truncate text-xs"
                :class="row.done ? 'text-muted-foreground line-through' : ''"
              >
                {{ row.title || t('item.untitled') }}
              </span>
              <span
                v-if="projectName(row.projectId)"
                class="block truncate text-[10px] text-muted-foreground/70"
              >
                {{ projectName(row.projectId) }}
              </span>
            </button>

            <button
              v-if="row.projectId && row.runId"
              type="button"
              class="shrink-0 cursor-pointer rounded p-1 text-muted-foreground/60 opacity-0 transition hover:bg-accent hover:text-primary group-hover:opacity-100"
              :title="kind === 'note' ? t('item.openRunNoteTitle') : t('item.openRunTodoTitle')"
              @click="openLinkedRun(row)"
            >
              <MessageSquare class="h-3.5 w-3.5" :stroke-width="1.75" />
            </button>

            <button
              type="button"
              class="shrink-0 cursor-pointer rounded p-1 text-muted-foreground/60 opacity-0 transition hover:bg-accent hover:text-destructive group-hover:opacity-100"
              :title="t('common.delete')"
              @click="remove(row)"
            >
              <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
            </button>
          </li>
        </ul>
      </div>
    </div>
  </Drawer>
</template>
