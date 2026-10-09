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
//
// The Slack and Web tabs list captures pushed in by the Chrome extension (Inbox
// API). They are not written by hand: "New" becomes "Import .zip/.md", a row opens
// the capture's own detail window (lib/captureWindow), and the backend's
// `captures://changed` event drives the refetch. Being bulk inbox items, they also
// get a select mode for multi-delete. Notes and captures share one card (title,
// meta, preview, project · age footer) and its inline rename (pencil / F2).
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { emit, listen, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { open as openDialog } from '@tauri-apps/plugin-dialog'
import { openUrl } from '@tauri-apps/plugin-opener'
import {
  Check,
  ExternalLink,
  Folder,
  ListChecks,
  ListTodo,
  MessageSquare,
  Paperclip,
  Pencil,
  Plus,
  Search,
  Globe,
  Slack,
  StickyNote,
  Trash2,
  Upload,
  X,
} from 'lucide-vue-next'
import { AppSelect, Badge, Button, ConfirmModal, Input, ToastHost } from '@/components/ui'
import InlineRenameInput from '@/components/InlineRenameInput.vue'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { useProjectsStore } from '@/stores/projects'
import { useNotesStore } from '@/stores/notes'
import { useTodosStore } from '@/stores/todos'
import {
  CAPTURES_CHANGED,
  captureDomain,
  captureOriginalUrl,
  channelLabel,
  useCapturesStore,
  type Capture,
  type CaptureKind,
} from '@/stores/captures'
import {
  ITEM_CHANGED,
  openItemCreateWindow,
  openItemEditWindow,
} from '@/lib/itemWindow'
import {
  ITEM_LIST_OPEN_RUN,
  ITEM_LIST_SET_KIND,
  type ItemListKind,
  type ItemListOpenRun,
  type ItemListSetKind,
} from '@/lib/itemListWindow'
import { openCaptureWindow } from '@/lib/captureWindow'
import { relativeAge } from '@/lib/itemText'
import { refreshTray } from '@/lib/tray'

const { t } = useI18n()
const { confirm } = useConfirm()
const { toast } = useToast()
const todos = useTodosStore()
const notes = useNotesStore()
const captures = useCapturesStore()
const projects = useProjectsStore()

const params = new URLSearchParams(window.location.search)
const KINDS: ItemListKind[] = ['todo', 'note', 'slack', 'web']
function asKind(value: unknown): ItemListKind | null {
  return KINDS.includes(value as ItemListKind) ? (value as ItemListKind) : null
}
const kind = ref<ItemListKind>(asKind(params.get('kind')) ?? 'todo')
/** The capture kind of the active tab, or null on the todo / note tabs. */
const captureKind = computed<CaptureKind | null>(() =>
  kind.value === 'slack' || kind.value === 'web' ? kind.value : null,
)
const isCapture = computed(() => captureKind.value !== null)
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

const TABS: { key: ItemListKind; labelKey: string; icon: typeof ListTodo }[] = [
  { key: 'todo', labelKey: 'item.tabTodo', icon: ListTodo },
  { key: 'note', labelKey: 'item.tabNote', icon: StickyNote },
  { key: 'slack', labelKey: 'capture.slack.tab', icon: Slack },
  { key: 'web', labelKey: 'capture.web.tab', icon: Globe },
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
  /** Captures only: Slack `#channel · workspace`, web domain. */
  source: string
  /** Captures only: number of bundled attachments. */
  attachments: number
  /** Captures only: Slack permalink / page URL ("Open in Slack" / "Open original"). */
  url: string | null
  /** Captures only: the user renamed it (kept on re-export). */
  custom?: boolean
  /** Web only: just the user's selection was captured. */
  excerpt?: boolean
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
        source: '',
        attachments: 0,
        url: null,
      }))
    : captureKind.value
      ? captures.ofKind(captureKind.value).map(captureRow)
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
          source: '',
          attachments: 0,
          url: null,
        }
      }),
)

function captureRow(c: Capture): Row {
  const source =
    c.kind === 'slack'
      ? [channelLabel(c), c.workspace ?? ''].filter(Boolean).join(' · ')
      : captureDomain(c) || c.siteName?.trim() || ''
  return {
    id: c.id,
    title: c.title.trim() || firstLine(c.content) || t(`capture.${c.kind}.untitled`),
    // The source is searchable too (Slack channel / web domain + site name).
    body: `${c.content}\n${source}\n${c.siteName ?? ''}`,
    preview: noteSnippet(c.content, true),
    done: false,
    projectId: c.projectId,
    runId: null,
    // A re-pushed capture bubbles up as fresh, so age it by its last update.
    createdAt: c.updatedAt,
    source,
    attachments: c.attachmentCount,
    url: captureOriginalUrl(c),
    custom: c.titleCustom,
    excerpt: c.kind === 'web' && c.selection,
  }
}

/** Web card avatar: the domain's first letter (no favicon fetch). */
function avatarLetter(row: Row): string {
  return (row.source || row.title || '?').charAt(0).toUpperCase()
}

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
  return relativeAge(iso)
}

/** Pull the current rows from the DB (the item window may have changed them). */
function refresh() {
  if (kind.value === 'todo') todos.fetchTodos()
  else if (kind.value === 'note') notes.fetchNotes()
  else captures.fetchCaptures()
}

function openCreate() {
  const k = kind.value
  if (k === 'slack' || k === 'web') void importCaptures()
  else openItemCreateWindow(k, { projectId: contextProjectId.value })
}

function openItem(id: string) {
  const k = kind.value
  if (k === 'slack' || k === 'web') {
    if (selecting.value) toggleSelected(id)
    else openCaptureWindow(id, k)
  } else {
    openItemEditWindow(k, id)
  }
}

// ── Captures (Slack / Web) ──────────────────────────────────────────────────

const importing = ref(false)

/**
 * Manual fallback for the Chrome extension: pick exported .zip / .md files and
 * feed them through the same parser as the Inbox API.
 */
async function importCaptures() {
  const target = captureKind.value
  if (!target || importing.value) return
  let picked: string | string[] | null
  try {
    picked = await openDialog({
      multiple: true,
      directory: false,
      filters: [{ name: t(`capture.${target}.importFilter`), extensions: ['zip', 'md', 'markdown'] }],
    })
  } catch (e) {
    toast.error(String(e))
    return
  }
  const paths = picked == null ? [] : Array.isArray(picked) ? picked : [picked]
  if (paths.length === 0) return
  importing.value = true
  let created = 0
  let updated = 0
  try {
    for (const path of paths) {
      try {
        const res = await captures.importFile(path, target)
        if (res.status === 'updated') updated++
        else created++
      } catch (e) {
        const name = path.split(/[\\/]/).pop() ?? path
        toast.error(t('capture.importFailed', { name, error: String(e) }))
      }
    }
  } finally {
    importing.value = false
  }
  if (created || updated) toast.success(t('capture.imported', { created, updated }))
}

function openOriginal(row: Row) {
  if (!row.url) return
  openUrl(row.url).catch((e) => toast.error(String(e)))
}

/** The note / capture card whose title is being renamed inline (one at a time). */
const renamingId = ref<string | null>(null)
/** Cards carry a renamable title; a todo row is its text, edited in its window. */
const canRename = computed(() => kind.value !== 'todo')

function startRename(row: Row) {
  if (selecting.value || !canRename.value) return
  renamingId.value = row.id
}

/** The value the rename field starts from: a note's own title, not a derived one. */
function renameValue(row: Row): string {
  if (kind.value !== 'note') return row.title
  return notes.notes.find((n) => n.id === row.id)?.title.trim() ?? ''
}

/** Empty or unchanged → just close the field, no request. */
async function commitRename(row: Row, title: string) {
  renamingId.value = null
  if (!title || title === renameValue(row)) return
  if (kind.value === 'note') {
    const note = notes.notes.find((n) => n.id === row.id)
    if (!note) return
    await notes.update(row.id, title, note.content)
    emit(ITEM_CHANGED, { kind: 'note' })
    return
  }
  try {
    await captures.rename(row.id, title)
  } catch (e) {
    toast.error(t('capture.renameFailed', { error: String(e) }))
  }
}

function onCardKeydown(e: KeyboardEvent, row: Row) {
  if (e.isComposing) return
  if (e.key === 'F2') {
    e.preventDefault()
    startRename(row)
  } else if (e.key === 'Enter' || e.key === ' ') {
    e.preventDefault()
    openItem(row.id)
  }
}

/** Select mode (capture tabs): pick several threads, then delete them in one go. */
const selecting = ref(false)
const selected = ref<Set<string>>(new Set())

function toggleSelected(id: string) {
  const next = new Set(selected.value)
  if (next.has(id)) next.delete(id)
  else next.add(id)
  selected.value = next
}

function exitSelect() {
  selecting.value = false
  selected.value = new Set()
}

const allFilteredSelected = computed(
  () => filtered.value.length > 0 && filtered.value.every((row) => selected.value.has(row.id)),
)

function toggleSelectAll() {
  selected.value = allFilteredSelected.value
    ? new Set()
    : new Set(filtered.value.map((row) => row.id))
}

async function removeSelected() {
  const ids = [...selected.value]
  if (ids.length === 0) return
  if (
    !(await confirm({
      title: t(`capture.${captureKind.value}.confirm.deleteManyTitle`),
      message: t(`capture.${captureKind.value}.confirm.deleteManyMessage`, { count: ids.length }),
      confirmLabel: t('common.delete'),
    }))
  ) {
    return
  }
  try {
    await captures.removeMany(ids)
    exitSelect()
  } catch (e) {
    toast.error(String(e))
  }
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
  if (captureKind.value) {
    if (
      !(await confirm({
        title: t(`capture.${captureKind.value}.confirm.deleteTitle`),
        message: t(`capture.${captureKind.value}.confirm.deleteMessage`),
        confirmLabel: t('common.delete'),
      }))
    ) {
      return
    }
    try {
      await captures.removeMany([row.id])
    } catch (e) {
      toast.error(String(e))
    }
    return
  }
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
  kind.value === 'note'
    ? t('item.navNotes')
    : captureKind.value
      ? t(`capture.${captureKind.value}.nav`)
      : t('item.navTodos'),
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
  exitSelect()
  renamingId.value = null
  refresh()
})

// Rows that vanished (deleted elsewhere / filtered out) can't stay selected.
watch(filtered, (visibleRows) => {
  if (selected.value.size === 0) return
  const visible = new Set(visibleRows.map((row) => row.id))
  const kept = [...selected.value].filter((id) => visible.has(id))
  if (kept.length !== selected.value.size) selected.value = new Set(kept)
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
        const next = asKind(payload.kind)
        if (next) kind.value = next
        if (payload.projectId !== undefined) contextProjectId.value = payload.projectId ?? null
      }),
      // A capture arrived over the Inbox API / was changed in its detail window.
      await listen(CAPTURES_CHANGED, () => {
        if (isCapture.value) captures.fetchCaptures()
      }),
      // MCP sidecar writes (slack_threads_* / web_pages_*) emit no Tauri event:
      // catch up on focus.
      await getCurrentWindow().onFocusChanged(({ payload: focused }) => {
        if (focused && isCapture.value) captures.fetchCaptures()
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
    <!-- Header: the tabs (and, on capture tabs, the select toggle).
         Creating — New todo / New note / Import — is the footer's job. -->
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
      </div>
      <button
        v-if="captureKind && rows.length > 0"
        type="button"
        class="flex h-7 w-7 shrink-0 cursor-pointer items-center justify-center rounded-md transition-colors"
        :class="selecting
          ? 'bg-primary/15 text-primary hover:bg-primary/25'
          : 'text-muted-foreground hover:bg-accent hover:text-foreground'"
        :title="selecting ? t('capture.exitSelect') : t(`capture.${captureKind}.select`)"
        @click="selecting ? exitSelect() : (selecting = true)"
      >
        <ListChecks class="h-3.5 w-3.5" :stroke-width="1.75" />
      </button>
    </div>

    <!-- Capture select mode: bulk actions over the picked rows. -->
    <div
      v-if="isCapture && selecting"
      class="flex items-center gap-2 border-b border-border/60 bg-accent/30 px-3 h-10 shrink-0 text-xs"
    >
      <button
        type="button"
        class="cursor-pointer text-muted-foreground hover:text-foreground"
        @click="toggleSelectAll"
      >
        {{ allFilteredSelected ? t('capture.selectNone') : t('capture.selectAll') }}
      </button>
      <span class="tabular-nums text-muted-foreground">
        {{ t('capture.selectedCount', { count: selected.size }) }}
      </span>
      <Button
        variant="destructive"
        size="xs"
        class="ml-auto"
        :disabled="selected.size === 0"
        @click="removeSelected"
      >
        <Trash2 class="h-3 w-3" :stroke-width="1.75" />
        {{ t('common.delete') }}
      </Button>
      <Button variant="ghost" size="xs" @click="exitSelect">
        {{ t('common.cancel') }}
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
              :is="kind === 'todo' ? ListTodo : kind === 'slack' ? Slack : kind === 'web' ? Globe : StickyNote"
              class="h-5 w-5 text-muted-foreground"
              :stroke-width="1.5"
            />
          </div>
          <p class="text-xs text-muted-foreground">
            {{ kind === 'todo' ? t('item.emptyTodo') : captureKind ? t(`capture.${captureKind}.empty`) : t('item.emptyNote') }}
          </p>
          <p class="mt-1 max-w-xs text-[11px] text-muted-foreground/70">
            {{ captureKind ? t(`capture.${captureKind}.emptyHint`) : kind === 'todo' ? t('item.emptyTodoHint') : t('item.emptyNoteHint') }}
          </p>
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

            <!-- Note / capture: a card — title, then (Slack) `#channel · workspace`
                 / (web) avatar + `domain · age` (+ Excerpt), a preview, then
                 attachments · project · age. In select mode a click toggles it. -->
            <!-- A focusable div rather than a <button>: it hosts the inline rename
                 input, which can't live inside a button. F2 renames the focused card. -->
            <div
              v-else
              role="button"
              tabindex="0"
              class="flex w-full cursor-pointer items-start gap-2.5 rounded-lg border bg-card px-3 py-2.5 text-left outline-none transition-colors hover:bg-accent/40 focus-visible:ring-1 focus-visible:ring-ring"
              :class="selecting && selected.has(row.id)
                ? 'border-primary/60 bg-primary/5'
                : 'border-border/70 hover:border-border'"
              :title="selecting || renamingId === row.id ? undefined : t('item.openTitle')"
              @click="renamingId !== row.id && openItem(row.id)"
              @keydown.self="onCardKeydown($event, row)"
            >
              <span
                v-if="selecting"
                class="mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center rounded-[5px] border transition-colors"
                :class="selected.has(row.id)
                  ? 'bg-primary border-primary text-primary-foreground'
                  : 'border-muted-foreground/40'"
              >
                <Check v-if="selected.has(row.id)" class="h-2.5 w-2.5" :stroke-width="3" />
              </span>
              <!-- Web: the domain's letter as an avatar (no favicon fetch). -->
              <span
                v-if="kind === 'web'"
                class="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-md bg-muted text-xs font-semibold text-muted-foreground"
                aria-hidden="true"
              >
                {{ avatarLetter(row) }}
              </span>
              <span class="block min-w-0 flex-1">
                <InlineRenameInput
                  v-if="renamingId === row.id"
                  class="w-full text-[13px] font-medium leading-snug"
                  :value="renameValue(row)"
                  :placeholder="captureKind ? t(`capture.${captureKind}.renamePlaceholder`) : t('item.titlePlaceholder')"
                  @commit="commitRename(row, $event)"
                  @cancel="renamingId = null"
                />
                <span v-else class="flex min-w-0 items-center gap-1.5">
                  <span
                    class="block truncate text-[13px] font-medium leading-snug text-foreground"
                    :title="row.custom ? t('capture.renamedHint') : undefined"
                  >
                    {{ row.title || t('item.untitled') }}
                  </span>
                  <Badge v-if="row.excerpt" tone="info" size="xs" class="shrink-0" :title="t('capture.excerptHint')">
                    {{ t('capture.excerpt') }}
                  </Badge>
                </span>
                <span
                  v-if="kind === 'web'"
                  class="mt-0.5 block truncate text-[11px] text-muted-foreground"
                >
                  {{ [row.source, relativeTime(row.createdAt)].filter(Boolean).join(' · ') }}
                </span>
                <span v-else-if="row.source" class="mt-0.5 block truncate text-[11px] text-muted-foreground">
                  {{ row.source }}
                </span>
                <span
                  v-if="row.preview"
                  class="mt-1 line-clamp-2 text-xs leading-relaxed text-muted-foreground"
                >
                  {{ row.preview }}
                </span>
                <!-- Web carries its age on the domain line, so this line only shows
                     when there is something else to say. -->
                <span
                  v-if="kind !== 'web' || row.attachments > 0 || projectName(row.projectId)"
                  class="mt-2 flex items-center gap-1.5 text-[10px] text-muted-foreground/70"
                >
                  <template v-if="row.attachments > 0">
                    <Paperclip class="h-3 w-3 shrink-0" :stroke-width="1.75" />
                    <span class="shrink-0 tabular-nums">{{ row.attachments }}</span>
                    <span v-if="kind !== 'web' || projectName(row.projectId)" aria-hidden="true">·</span>
                  </template>
                  <template v-if="projectName(row.projectId)">
                    <Folder class="h-3 w-3 shrink-0" :stroke-width="1.75" />
                    <span class="truncate">{{ projectName(row.projectId) }}</span>
                    <span v-if="kind !== 'web'" aria-hidden="true">·</span>
                  </template>
                  <span v-if="kind !== 'web'" class="shrink-0 tabular-nums">{{ relativeTime(row.createdAt) }}</span>
                </span>
              </span>
            </div>

            <!-- Row actions float over the right edge on hover, so they take no
                 space at rest and every row keeps the same width. -->
            <div
              v-if="!(selecting || renamingId === row.id)"
              class="pointer-events-none absolute flex items-center gap-0.5 rounded-md border border-border/60 bg-popover p-0.5 opacity-0 shadow-sm transition-opacity group-hover:pointer-events-auto group-hover:opacity-100 focus-within:pointer-events-auto focus-within:opacity-100"
              :class="kind === 'todo' ? 'right-1.5 top-1/2 -translate-y-1/2' : 'right-2 top-2'"
            >
              <button
                v-if="canRename"
                type="button"
                class="cursor-pointer rounded p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                :title="t('capture.rename')"
                @click="startRename(row)"
              >
                <Pencil class="h-3.5 w-3.5" :stroke-width="1.75" />
              </button>
              <button
                v-if="isCapture && row.url"
                type="button"
                class="cursor-pointer rounded p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-primary"
                :title="kind === 'web' ? t('capture.openOriginal') : t('capture.openInSlack')"
                @click="openOriginal(row)"
              >
                <ExternalLink class="h-3.5 w-3.5" :stroke-width="1.75" />
              </button>
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

    <!-- Footer: the item count on the left ("shown / total" while filtered),
         the tab's create action on the right, laid out like the detail
         windows' action bar. Captures are not written by hand, so their tabs
         import an exported .zip / .md instead. -->
    <div class="flex items-center gap-2 border-t border-border/60 px-3 py-2.5 shrink-0">
      <span class="mr-auto min-w-0 truncate text-[11px] tabular-nums text-muted-foreground">
        {{
          filtered.length === rows.length
            ? t('item.count', { count: rows.length }, rows.length)
            : t('item.countFiltered', { shown: filtered.length, total: rows.length }, rows.length)
        }}
      </span>
      <Button
        size="sm"
        :disabled="isCapture && importing"
        :title="captureKind ? t(`capture.${captureKind}.importTitle`) : undefined"
        @click="openCreate"
      >
        <component :is="isCapture ? Upload : Plus" class="h-3.5 w-3.5" :stroke-width="2" />
        {{ isCapture ? t('capture.import') : kind === 'todo' ? t('item.newTodo') : t('item.newNote') }}
      </Button>
    </div>

    <!-- The pop-out doesn't mount the main app's dialog/toast hosts. -->
    <ConfirmModal />
    <ToastHost />
  </div>
</template>
