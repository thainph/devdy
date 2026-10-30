<script setup lang="ts">
// THE Notes window or THE Todos window (opened via lib/itemListWindow,
// `?itemListWindow=1&kind=note|todo[&projectId=][&id=]`) — one window per kind,
// laid out like the Slack threads window:
//
//   ┌ (native OS titlebar only: "Notes: <title>" / "Todos") ────────────────┐
//   ├ sidebar: search · pin · reload,        │ detail header: title /        │
//   │ project filter + New, segmented filter │ meta · [project][Sessions] |  │
//   │ (notes All|Linked, todos Open|Done|All)│ (✓ Done) ✎ (⧉) ⋯              │
//   │ items grouped by project               │ body: markdown, or the inline │
//   │                                        │ editor while editing          │
//   └────────────────────────────────────────┴───────────────────────────────┘
//
// Reading and editing both happen here: ✎ edits inline, ⌘/Ctrl+Enter saves,
// Esc cancels, clicking a note's title renames it (a todo: edits). Creating
// stays in the capture window (lib/itemWindow, "+ New"). Every "open this
// note / todo" in the app lands here with `id` (or SELECT for an open window).
//
// Its own webview means its own store copy: ITEM_CHANGED (from the capture
// window, the backend's link commands, the other windows) refetches; the MCP
// sidecar writes without events, so focusing the window reloads too.
// Sessions live in the main window: opening a linked one or starting a new one
// is a request to it (WINDOW_OPEN_RUN / WINDOW_NEW_SESSION, App.vue).
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { emit, listen, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { ListTodo, StickyNote } from 'lucide-vue-next'
import { ConfirmModal, ToastHost } from '@/components/ui'
import ItemSidebar from '@/components/items/ItemSidebar.vue'
import ItemHeader from '@/components/items/ItemHeader.vue'
import ItemBody from '@/components/items/ItemBody.vue'
import { noteRow, todoRow, type ItemRow } from '@/components/items/itemRow'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { useFloatingWindow } from '@/composables/useFloatingWindow'
import { useProjectsStore } from '@/stores/projects'
import { useNotesStore, type Note } from '@/stores/notes'
import { useTodosStore, type Todo } from '@/stores/todos'
import { errorCode } from '@/stores/slackThreads'
import { ITEM_CHANGED, openItemCreateWindow, type ItemKind } from '@/lib/itemWindow'
import { itemListSelectEvent, type ItemListSelect } from '@/lib/itemListWindow'
import {
  WINDOW_NEW_SESSION,
  WINDOW_OPEN_RUN,
  linkReference,
  linkTargetRun,
  listTargetRuns,
  unlinkTargetRun,
  type SessionLink,
  type WindowNewSession,
  type WindowOpenRun,
} from '@/lib/sessionLinks'
import { firstLine } from '@/lib/itemText'
import { refreshTray } from '@/lib/tray'

const { t } = useI18n()
const { toast } = useToast()
const { confirm } = useConfirm()
const notes = useNotesStore()
const todos = useTodosStore()
const projects = useProjectsStore()
const { pinned, togglePin } = useFloatingWindow()

const params = new URLSearchParams(window.location.search)
/** The kind this window shows — fixed for its lifetime (one window per kind). */
const kind: ItemKind = params.get('kind') === 'note' ? 'note' : 'todo'
const initialId = params.get('id') || null
/** The project the window was opened from: the default filter and "+ New" project. */
const contextProjectId = ref<string | null>(params.get('projectId') || null)

// ── Sidebar: search + project filter + kind filter (all client-side) ───────

const NO_PROJECT = '__none__'
const search = ref('')
/** '' = all projects, NO_PROJECT = items without one, else a project id. */
const projectFilter = ref<string>(contextProjectId.value ?? '')
/** notes: '' | 'linked'; todos: 'active' | 'done' | 'all'. */
const filter = ref<string>(kind === 'note' ? '' : 'active')

const loadError = computed(() => (kind === 'note' ? notes.error : todos.error))

const allRows = computed<ItemRow[]>(() =>
  kind === 'note' ? notes.notes.map(noteRow) : todos.todos.map(todoRow),
)

function matchesFilters(row: ItemRow): boolean {
  if (filter.value === 'linked' && row.linkedRunCount === 0) return false
  if (filter.value === 'active' && row.done) return false
  if (filter.value === 'done' && !row.done) return false
  if (projectFilter.value === NO_PROJECT) return !row.projectId
  if (projectFilter.value) return row.projectId === projectFilter.value
  return true
}

const visibleRows = computed(() => {
  const q = search.value.trim().toLowerCase()
  return allRows.value.filter((row) => matchesFilters(row) && (!q || row.hay.includes(q)))
})

const projectNames = computed<Record<string, string>>(() =>
  Object.fromEntries(projects.projects.map((p) => [p.id, p.name])),
)

const projectOptions = computed(() => {
  const ids = new Set<string>()
  for (const row of allRows.value) if (row.projectId) ids.add(row.projectId)
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

const emptyStore = computed(() => allRows.value.length === 0)

// ── Selection ───────────────────────────────────────────────────────────────

const selectedId = ref<string | null>(null)
const item = computed<Note | Todo | null>(() => {
  const id = selectedId.value
  if (!id) return null
  return kind === 'note'
    ? notes.notes.find((n) => n.id === id) ?? null
    : todos.todos.find((x) => x.id === id) ?? null
})

/** Sessions linked to the selected item. */
const links = ref<SessionLink[]>([])
let linksSeq = 0
async function loadLinks() {
  const id = selectedId.value
  const seq = ++linksSeq
  if (!id) {
    links.value = []
    return
  }
  try {
    const list = await listTargetRuns({ kind, id })
    if (seq === linksSeq) links.value = list
  } catch {
    /* an extra: keep what is shown */
  }
}

const editing = ref(false)
const renaming = ref(false)
const bodyRef = ref<InstanceType<typeof ItemBody> | null>(null)

/** Leaving unsaved edits takes a confirm; true = go ahead. */
async function confirmDiscard(): Promise<boolean> {
  if (!editing.value || !bodyRef.value?.dirty) return true
  return confirm({
    title: t('item.confirm.discardTitle'),
    message: t('item.confirm.discardMessage'),
    confirmLabel: t('item.confirm.discard'),
  })
}

async function select(id: string | null) {
  if (id === selectedId.value) return
  if (!(await confirmDiscard())) return
  editing.value = false
  renaming.value = false
  selectedId.value = id
  if (id && !visibleRows.value.some((r) => r.id === id)) revealRow(id)
  links.value = []
  void loadLinks()
}

/** Make a row visible under the filters (an item opened from elsewhere). */
function revealRow(id: string) {
  const row = allRows.value.find((r) => r.id === id)
  if (!row) return
  search.value = ''
  if (!matchesFilters(row)) {
    projectFilter.value = ''
    filter.value = kind === 'note' ? '' : 'all'
  }
}

// The URL follows the selection, so a reload reopens it.
watch(selectedId, (id) => {
  const url = new URL(window.location.href)
  if (id) url.searchParams.set('id', id)
  else url.searchParams.delete('id')
  window.history.replaceState(null, '', url.toString())
})

// The selected item vanished (deleted here, elsewhere or by the AI). An
// unsaved draft is not dropped silently: it goes to the clipboard.
watch(item, (now, before) => {
  if (!now && before && selectedId.value) {
    if (editing.value && bodyRef.value?.dirty) void rescueDraft(bodyRef.value.draftText())
    editing.value = false
    selectedId.value = visibleRows.value[0]?.id ?? null
    void loadLinks()
  }
})

async function rescueDraft(text: string) {
  try {
    await navigator.clipboard.writeText(text)
    toast.info(kind === 'note' ? t('item.deletedNoteDraftCopied') : t('item.deletedTodoDraftCopied'))
  } catch {
    toast.error(kind === 'note' ? t('item.deletedNoteDraftLost') : t('item.deletedTodoDraftLost'))
  }
}

// ── Writes ──────────────────────────────────────────────────────────────────

/** Tell the other webviews (other windows, the main window's picker / menus). */
function announce() {
  emit(ITEM_CHANGED, { kind }).catch(() => {})
}

function startEdit() {
  if (!item.value) return
  renaming.value = false
  editing.value = true
}

// Esc / Cancel: a stray Esc must not silently drop a hand-written draft.
async function cancelEdit() {
  if (!(await confirmDiscard())) return
  editing.value = false
}

function onSaved() {
  editing.value = false
  announce()
}

function startRename() {
  if (!item.value) return
  if (kind === 'note') renaming.value = true
  else startEdit()
}

async function commitRename(title: string) {
  renaming.value = false
  const n = item.value as Note | null
  if (!n || !title || title === n.title) return
  await notes.update(n.id, title, n.content)
  announce()
}

async function setProject(next: string | null) {
  const it = item.value
  if (!it || next === it.project_id) return
  if (kind === 'note') await notes.setProject(it.id, next)
  else await todos.setProject(it.id, next)
  announce()
}

async function setDueDate(next: string | null) {
  const it = item.value as Todo | null
  if (kind !== 'todo' || !it || next === it.due_date) return
  await todos.setDueDate(it.id, next)
  announce()
}

async function toggleDone(id: string) {
  if (kind !== 'todo') return
  await todos.toggle(id)
  announce()
}

async function remove() {
  const it = item.value
  if (!it) return
  const isNote = kind === 'note'
  const ok = await confirm({
    title: isNote ? t('item.confirm.deleteNoteTitle') : t('item.confirm.deleteTodoTitle'),
    message: isNote ? t('item.confirm.deleteNoteMessage') : t('item.confirm.deleteTodoMessage'),
    confirmLabel: t('common.delete'),
  })
  if (!ok) return
  // The next row takes the selection.
  const rows = visibleRows.value
  const i = rows.findIndex((r) => r.id === it.id)
  const next = rows[i + 1]?.id ?? rows[i - 1]?.id ?? null
  editing.value = false
  if (isNote) await notes.remove(it.id)
  else await todos.remove(it.id)
  selectedId.value = next
  void loadLinks()
  announce()
}

const copiedId = ref(false)
async function copyId() {
  const id = selectedId.value
  if (!id) return
  try {
    await navigator.clipboard.writeText(id)
    copiedId.value = true
    setTimeout(() => (copiedId.value = false), 1500)
    toast.success(t('item.idCopied'))
  } catch {
    /* clipboard unavailable */
  }
}

function openCreate() {
  const scoped = projectFilter.value && projectFilter.value !== NO_PROJECT ? projectFilter.value : null
  void openItemCreateWindow(kind, { projectId: scoped ?? contextProjectId.value })
}

// ── Linked sessions ─────────────────────────────────────────────────────────

function openRun(link: SessionLink) {
  emit(WINDOW_OPEN_RUN, { projectId: link.projectId, runId: link.runId } satisfies WindowOpenRun).catch(
    () => {},
  )
}

async function unlinkRun(link: SessionLink) {
  const id = selectedId.value
  if (!id) return
  const snapshot = links.value.slice()
  links.value = links.value.filter((l) => l.runId !== link.runId)
  try {
    await unlinkTargetRun({ kind, id }, link.runId)
  } catch (e) {
    links.value = snapshot
    toast.error(t('sessionLink.unlinkFailed', { error: errorCode(e).detail }))
  }
}

async function linkRun(runId: string) {
  const id = selectedId.value
  if (!id) return
  try {
    await linkTargetRun({ kind, id }, runId, 'manual')
    await loadLinks()
  } catch (e) {
    toast.error(t('sessionLink.linkFailed', { error: errorCode(e).detail }))
  }
}

/**
 * Ask the main window for a new session in the item's project, its composer
 * prefilled with the item's reference; it links the item once the run exists.
 */
function newSession() {
  const it = item.value
  if (!it) return
  if (!it.project_id) {
    toast.info(t('item.needProject'))
    return
  }
  const n = kind === 'note' ? (it as Note) : null
  const title = n ? n.title.trim() || firstLine(n.content) || t('item.untitled') : undefined
  emit(WINDOW_NEW_SESSION, {
    projectId: it.project_id,
    text: linkReference(kind, it.id, title),
    target: { kind, id: it.id },
  } satisfies WindowNewSession).catch((e) => toast.error(String(e)))
}

// ── Reload / events ─────────────────────────────────────────────────────────

const reloading = ref(false)
async function refresh() {
  if (kind === 'note') await notes.fetchNotes()
  else await todos.fetchTodos()
}

async function reloadAll() {
  reloading.value = true
  try {
    await Promise.all([refresh(), loadLinks()])
  } finally {
    reloading.value = false
  }
}

let reloadTimer: ReturnType<typeof setTimeout> | null = null
function scheduleReload() {
  if (reloadTimer) clearTimeout(reloadTimer)
  reloadTimer = setTimeout(() => void reloadAll(), 250)
}

// The OS window title doubles as this window's row in the menu-bar switcher
// (lib/tray.ts): "Notes: <title>" for the open note, "Todos" for the todos.
const windowTitle = computed(() => {
  if (kind === 'todo') return t('item.navTodos')
  const n = item.value as Note | null
  const name = n ? n.title.trim() || firstLine(n.content) : ''
  return name ? t('item.notesWindowWith', { name }) : t('item.navNotes')
})
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

let unlisten: UnlistenFn[] = []
/** The close was confirmed: let the next close request through. */
let closing = false

/**
 * Select an item asked for from elsewhere: one created a moment ago may not be
 * in this window's store yet, so refetch first.
 */
async function selectFresh(id: string) {
  if (!allRows.value.some((r) => r.id === id)) await refresh()
  await select(id)
}

onMounted(async () => {
  void projects.fetchProjects()
  try {
    unlisten.push(
      // A write elsewhere (capture window, link commands, other windows).
      await listen<{ kind?: string }>(ITEM_CHANGED, (e) => {
        if (!e.payload?.kind || e.payload.kind === kind) scheduleReload()
      }),
    )
    unlisten.push(
      // Reopened from the app: re-point the project scope and / or select an item.
      await listen<ItemListSelect>(itemListSelectEvent(kind), (e) => {
        const p = e.payload
        if (!p) return
        if (p.projectId !== undefined) {
          contextProjectId.value = p.projectId ?? null
          projectFilter.value = p.projectId ?? ''
        }
        if (p.id) void selectFresh(p.id)
      }),
    )
    // Closing with an unsaved draft asks first. A listener here makes the close
    // ours: unless prevented, the window is destroyed after it (app quit via
    // app.exit() is not routed through it).
    unlisten.push(
      await getCurrentWindow().onCloseRequested(async (e) => {
        if (closing || !editing.value || !bodyRef.value?.dirty) return
        e.preventDefault()
        if (!(await confirmDiscard())) return
        closing = true
        await getCurrentWindow().destroy()
      }),
    )
    // MCP sidecar writes (notes_* / todos_*) emit no Tauri event.
    unlisten.push(
      await getCurrentWindow().onFocusChanged(({ payload: focused }) => {
        if (focused && !editing.value) void reloadAll()
      }),
    )
  } catch {
    /* running outside the Tauri shell */
  }
  await refresh()
  await nextTick()
  // A stale `id` (deleted since) falls back to the first row.
  const wanted = initialId && allRows.value.some((r) => r.id === initialId) ? initialId : null
  const first = selectedId.value ?? wanted ?? visibleRows.value[0]?.id ?? null
  if (first) await select(first)
})

onBeforeUnmount(() => {
  if (reloadTimer) clearTimeout(reloadTimer)
  unlisten.forEach((off) => off())
  unlisten = []
})
</script>

<template>
  <div class="flex h-screen w-screen overflow-hidden bg-background text-foreground">
    <ItemSidebar
      v-model:search="search"
      v-model:project="projectFilter"
      v-model:filter="filter"
      :kind="kind"
      :rows="visibleRows"
      :empty-store="emptyStore"
      :selected-id="selectedId"
      :project-options="projectOptions"
      :project-names="projectNames"
      :error="loadError"
      :pinned="pinned"
      :reloading="reloading"
      @select="select"
      @add="openCreate"
      @toggle-done="toggleDone"
      @toggle-pin="togglePin"
      @reload="reloadAll"
      @retry="reloadAll"
    />

    <section v-if="item" class="flex min-w-0 flex-1 flex-col">
      <ItemHeader
        :kind="kind"
        :item="item"
        :links="links"
        :renaming="renaming"
        :editing="editing"
        :can-save="!!bodyRef?.canSave"
        :saving="!!bodyRef?.saving"
        :copied-id="copiedId"
        @start-rename="startRename"
        @commit-rename="commitRename"
        @cancel-rename="renaming = false"
        @edit="startEdit"
        @cancel-edit="cancelEdit"
        @save="bodyRef?.save()"
        @toggle-done="toggleDone(item.id)"
        @set-project="setProject"
        @set-due-date="setDueDate"
        @open-run="openRun"
        @unlink-run="unlinkRun"
        @link-run="linkRun"
        @new-session="newSession"
        @copy-id="copyId"
        @delete="remove"
      />
      <ItemBody
        ref="bodyRef"
        :kind="kind"
        :item="item"
        :editing="editing"
        @saved="onSaved"
        @cancel="cancelEdit"
      />
    </section>
    <div v-else class="flex min-w-0 flex-1 items-center justify-center p-6">
      <div class="text-center">
        <div class="mx-auto mb-3 flex h-10 w-10 items-center justify-center rounded-xl bg-muted">
          <component
            :is="kind === 'note' ? StickyNote : ListTodo"
            class="h-5 w-5 text-muted-foreground"
            :stroke-width="1.5"
          />
        </div>
        <p class="text-xs text-muted-foreground">
          {{ kind === 'note' ? t('item.noSelectionNote') : t('item.noSelectionTodo') }}
        </p>
      </div>
    </div>

    <!-- The pop-out doesn't mount the main app's dialog/toast hosts. -->
    <ConfirmModal />
    <ToastHost />
  </div>
</template>
