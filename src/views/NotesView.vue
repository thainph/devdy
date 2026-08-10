<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue'
import { useNotesStore } from '@/stores/notes'
import { useProjectsStore } from '@/stores/projects'
import { Button, Card, Drawer, Input, AppSelect } from '@/components/ui'
import MarkdownPreview from '@/components/MarkdownPreview.vue'
import { useConfirm } from '@/composables/useConfirm'
import { useMarkdown } from '@/lib/markdown'
import { openUrl } from '@tauri-apps/plugin-opener'
import { Plus, GripVertical, Trash2, StickyNote, Pencil, Search, X, FolderOpen } from 'lucide-vue-next'

const store = useNotesStore()
const projectsStore = useProjectsStore()
const { confirm } = useConfirm()
const { renderText, loadMarkdown } = useMarkdown()

// Sentinel values for the project filter/select. Real project ids are UUIDs so
// they never collide with these.
const FILTER_ALL = ''
const FILTER_NONE = '__none__'
const PROJECT_NONE = '' // "no project" in the add form / detail select

onMounted(() => {
  store.fetchNotes()
  projectsStore.fetchProjects()
  loadMarkdown()
})

// --- Project helpers ----------------------------------------------------------
function projectName(id: string | null): string | null {
  if (!id) return null
  return projectsStore.projects.find(p => p.id === id)?.name ?? null
}

// Options for the add form / detail drawer: "no project" + every project.
const projectSelectOptions = computed(() => [
  { value: PROJECT_NONE, label: 'No project' },
  ...projectsStore.projects.map(p => ({ value: p.id, label: p.name })),
])

// Options for the filter bar: all / none / every project.
const projectFilterOptions = computed(() => [
  { value: FILTER_ALL, label: 'All projects' },
  { value: FILTER_NONE, label: 'No project' },
  ...projectsStore.projects.map(p => ({ value: p.id, label: p.name })),
])

// --- Search + filter ----------------------------------------------------------
const search = ref('')
const projectFilter = ref(FILTER_ALL)

const filteredNotes = computed(() => {
  const q = search.value.trim().toLowerCase()
  return store.notes.filter(n => {
    // Project filter
    if (projectFilter.value === FILTER_NONE) {
      if (n.project_id && projectName(n.project_id)) return false
    } else if (projectFilter.value !== FILTER_ALL) {
      if (n.project_id !== projectFilter.value) return false
    }
    // Text search (title first, content as a bonus)
    if (q) {
      const hay = `${n.title}\n${n.content}`.toLowerCase()
      if (!hay.includes(q)) return false
    }
    return true
  })
})

// Drag-reorder only makes sense against the full, unfiltered list — otherwise
// visible indices don't map to store indices. Disable it while filtering.
const isFiltered = computed(
  () => search.value.trim() !== '' || projectFilter.value !== FILTER_ALL,
)

function clearFilters() {
  search.value = ''
  projectFilter.value = FILTER_ALL
}

// A note's display title: explicit title, else first non-empty line of content.
function displayTitle(note: { title: string; content: string }): string {
  if (note.title.trim()) return note.title.trim()
  const firstLine = note.content.split('\n').find(l => l.trim())
  return firstLine?.trim() || 'Untitled note'
}

// --- Create drawer ------------------------------------------------------------
// Creating a note is its own focused surface (a right-side drawer), kept fully
// separate from the list. The list screen only browses; this drawer only adds.
const createOpen = ref(false)
const draftTitle = ref('')
const draftContent = ref('')
const draftProject = ref(PROJECT_NONE)
const createTitleRef = ref<{ $el?: HTMLInputElement } | null>(null)

function openCreate() {
  draftTitle.value = ''
  draftContent.value = ''
  // Prefill the project from the active filter so a burst of related notes
  // lands in the right place without re-picking every time.
  draftProject.value =
    projectFilter.value !== FILTER_ALL && projectFilter.value !== FILTER_NONE
      ? projectFilter.value
      : PROJECT_NONE
  createOpen.value = true
  nextTick(() => createTitleRef.value?.$el?.focus())
}

function closeCreate() {
  createOpen.value = false
}

async function handleAdd() {
  const title = draftTitle.value.trim()
  const content = draftContent.value.trim()
  if (!title && !content) return
  await store.add(title, content, draftProject.value || null)
  closeCreate()
}

// ⌘/Ctrl+Enter saves; Esc cancels. Plain Enter in the textarea inserts a
// newline (markdown notes are usually multi-line).
function onCreateKeydown(e: KeyboardEvent) {
  if (e.key === 'Enter' && (e.metaKey || e.ctrlKey) && !e.isComposing) {
    e.preventDefault()
    handleAdd()
  } else if (e.key === 'Escape') {
    e.preventDefault()
    closeCreate()
  }
}

// --- External links -----------------------------------------------------------
// Markdown links must not navigate the in-app webview; hand them to the OS
// browser. Returns true when the click was an anchor (handled).
function handleLinkClick(e: MouseEvent): boolean {
  const anchor = (e.target as HTMLElement).closest('a') as HTMLAnchorElement | null
  if (!anchor) return false
  e.preventDefault()
  const href = anchor.getAttribute('href') ?? ''
  if (/^(https?:|mailto:)/i.test(href)) openUrl(href).catch(() => { /* opener unavailable */ })
  return true
}

function onCardClick(id: string, e: MouseEvent) {
  if (handleLinkClick(e)) {
    e.stopPropagation()
    return
  }
  openDetail(id)
}

// --- Detail drawer ------------------------------------------------------------
const detailId = ref<string | null>(null)
const detailNote = computed(() => store.notes.find(n => n.id === detailId.value) ?? null)
const detailEditing = ref(false)
const editTitle = ref('')
const editContent = ref('')
const editProject = ref(PROJECT_NONE)

function openDetail(id: string) {
  detailId.value = id
  detailEditing.value = false
}

function closeDetail() {
  detailId.value = null
  detailEditing.value = false
}

function startDetailEdit() {
  if (!detailNote.value) return
  editTitle.value = detailNote.value.title
  editContent.value = detailNote.value.content
  editProject.value = detailNote.value.project_id ?? PROJECT_NONE
  detailEditing.value = true
}

function commitDetailEdit() {
  if (!detailId.value) return
  const title = editTitle.value.trim()
  const content = editContent.value.trim()
  if (!title && !content) return // empty save is a no-op; use Delete to remove
  store.update(detailId.value, title, content)
  const newProject = editProject.value || null
  if ((detailNote.value?.project_id ?? null) !== newProject) {
    store.setProject(detailId.value, newProject)
  }
  detailEditing.value = false
}

function cancelDetailEdit() {
  detailEditing.value = false
}

function onDetailEditKeydown(e: KeyboardEvent) {
  if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
    e.preventDefault()
    commitDetailEdit()
  } else if (e.key === 'Escape') {
    e.preventDefault()
    cancelDetailEdit()
  }
}

async function deleteFromDetail() {
  if (!detailNote.value) return
  if (!(await confirm({
    title: 'Delete note',
    message: 'Remove this note?',
    confirmLabel: 'Delete',
  }))) return
  const id = detailNote.value.id
  closeDetail()
  store.remove(id)
}

// --- Drag & drop reorder (pointer-based) --------------------------------------
// Same approach as TodosView: Tauri's webview swallows HTML5 DnD (reserved for
// OS file drops), so reorder uses raw pointer events. Only the grip starts it.
const dragIndex = ref<number | null>(null)
const dragOverIndex = ref<number | null>(null)

function indexFromPoint(x: number, y: number): number | null {
  const row = document.elementFromPoint(x, y)?.closest('[data-note-index]') as HTMLElement | null
  if (!row) return null
  const idx = Number(row.dataset.noteIndex)
  return Number.isNaN(idx) ? null : idx
}

function onDragMove(e: PointerEvent) {
  if (dragIndex.value === null) return
  const over = indexFromPoint(e.clientX, e.clientY)
  if (over !== null) dragOverIndex.value = over
}

function endDrag() {
  window.removeEventListener('pointermove', onDragMove)
  window.removeEventListener('pointerup', endDrag)
  document.body.style.userSelect = ''
  const from = dragIndex.value
  const to = dragOverIndex.value
  dragIndex.value = null
  dragOverIndex.value = null
  if (from !== null && to !== null && from !== to) {
    store.reorder(from, to)
  }
}

function startDrag(index: number, e: PointerEvent) {
  if (isFiltered.value) return
  e.preventDefault()
  dragIndex.value = index
  dragOverIndex.value = index
  document.body.style.userSelect = 'none'
  window.addEventListener('pointermove', onDragMove)
  window.addEventListener('pointerup', endDrag)
}

onBeforeUnmount(() => {
  window.removeEventListener('pointermove', onDragMove)
  window.removeEventListener('pointerup', endDrag)
})
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- Page header -->
    <div class="flex items-center justify-between px-6 h-13 border-b border-border/60 shrink-0">
      <div class="flex items-center gap-2">
        <h1 class="text-sm font-semibold">Notes</h1>
        <span
          v-if="store.notes.length > 0"
          class="flex h-4 min-w-4 items-center justify-center rounded-full bg-muted px-1.5 text-[10px] font-medium text-muted-foreground"
        >
          {{ store.notes.length }}
        </span>
      </div>
      <Button @click="openCreate">
        <Plus class="h-3.5 w-3.5" :stroke-width="2" />
        New note
      </Button>
    </div>

    <!-- Content -->
    <div class="flex-1 overflow-auto p-6">
      <div class="w-full">
        <!-- Search + project filter -->
        <div v-if="store.notes.length > 0" class="flex items-center gap-2 mb-3">
          <div class="relative flex-1">
            <Search class="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.75" />
            <Input
              v-model="search"
              placeholder="Search by title / content…"
              class="pl-8"
            />
            <button
              v-if="search"
              type="button"
              class="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground cursor-pointer"
              title="Clear search"
              @click="search = ''"
            >
              <X class="h-3.5 w-3.5" :stroke-width="2" />
            </button>
          </div>
          <div class="w-48 shrink-0">
            <AppSelect v-model="projectFilter" :options="projectFilterOptions" />
          </div>
        </div>

        <!-- Loading skeleton -->
        <div v-if="store.loading && store.notes.length === 0" class="space-y-2">
          <div v-for="i in 4" :key="i" class="h-20 rounded-lg border border-border bg-card animate-pulse" />
        </div>

        <!-- Error -->
        <div v-else-if="store.error" class="p-4 bg-destructive/10 text-destructive rounded-lg text-sm border border-destructive/20">
          {{ store.error }}
        </div>

        <!-- Empty state (no notes at all) -->
        <div
          v-else-if="store.notes.length === 0"
          class="flex flex-col items-center justify-center text-center min-h-72"
        >
          <div class="flex h-12 w-12 items-center justify-center rounded-xl bg-muted mb-4">
            <StickyNote class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
          </div>
          <p class="text-sm font-medium">No notes yet</p>
          <p class="text-xs text-muted-foreground mt-1 mb-4 max-w-56">Create your first note to jot things down quickly. Attach it to a project to filter later.</p>
          <Button size="md" @click="openCreate">
            <Plus class="h-3.5 w-3.5" :stroke-width="2" />
            Create note
          </Button>
        </div>

        <!-- No results (filtered) -->
        <div
          v-else-if="filteredNotes.length === 0"
          class="flex flex-col items-center justify-center text-center min-h-40"
        >
          <p class="text-sm font-medium">No notes found</p>
          <button
            type="button"
            class="text-xs text-primary hover:underline mt-1 cursor-pointer"
            @click="clearFilters"
          >
            Clear filters
          </button>
        </div>

        <!-- List -->
        <div v-else class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3 2xl:grid-cols-4">
          <Card
            v-for="(note, index) in filteredNotes"
            :key="note.id"
            :data-note-index="index"
            body-class="p-0"
            class="group cursor-pointer transition-colors hover:border-primary/40"
            :class="[
              dragOverIndex === index && dragIndex !== index ? 'ring-1 ring-primary/40' : '',
              dragIndex === index ? 'opacity-40' : '',
            ]"
            @click="onCardClick(note.id, $event)"
          >
            <div class="flex items-start gap-1.5 p-3">
              <!-- Drag handle (only when not filtering) -->
              <span
                v-if="!isFiltered"
                class="mt-0.5 shrink-0 cursor-grab text-muted-foreground/30 transition-colors group-hover:text-muted-foreground active:cursor-grabbing touch-none"
                title="Drag to reorder"
                @click.stop
                @pointerdown="startDrag(index, $event)"
              >
                <GripVertical class="h-4 w-4" :stroke-width="1.75" />
              </span>
              <div class="flex-1 min-w-0">
                <p class="text-sm font-medium truncate mb-1">{{ displayTitle(note) }}</p>
                <MarkdownPreview v-if="note.content" :html="renderText(note.content)" :max-height="88" />
                <div v-if="projectName(note.project_id)" class="mt-2 flex items-center gap-1 text-[11px] text-muted-foreground">
                  <FolderOpen class="h-3 w-3" :stroke-width="1.75" />
                  <span class="truncate">{{ projectName(note.project_id) }}</span>
                </div>
              </div>
            </div>
          </Card>
        </div>
      </div>
    </div>

    <!-- Create drawer (separate surface: only for adding new notes) -->
    <Drawer
      :open="createOpen"
      side="right"
      size="lg"
      @close="closeCreate"
    >
      <template #header>
        <Plus class="h-4 w-4 text-muted-foreground shrink-0" :stroke-width="1.75" />
        <h3 class="text-sm font-semibold flex-1 truncate">New note</h3>
      </template>

      <div class="p-5">
        <label class="block text-[11px] font-medium text-muted-foreground mb-1">Title</label>
        <Input
          ref="createTitleRef"
          v-model="draftTitle"
          placeholder="Note title…"
          class="mb-4 font-medium"
          @keydown="onCreateKeydown"
        />

        <label class="block text-[11px] font-medium text-muted-foreground mb-1">Content</label>
        <textarea
          v-model="draftContent"
          placeholder="Content (Markdown)…"
          class="w-full min-h-[45vh] resize-y rounded-md border border-border bg-background px-3 py-2 text-sm leading-relaxed font-mono focus:outline-none focus-visible:ring-1 focus-visible:ring-ring"
          @keydown="onCreateKeydown"
        />

        <div class="mt-4">
          <label class="block text-[11px] font-medium text-muted-foreground mb-1">Project</label>
          <AppSelect v-model="draftProject" :options="projectSelectOptions" placeholder="No project">
            <template #leading>
              <FolderOpen class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.75" />
            </template>
          </AppSelect>
        </div>

        <p class="mt-3 text-[11px] text-muted-foreground">
          Markdown supported · <span class="font-medium">⌘/Ctrl+Enter</span> to save · <span class="font-medium">Esc</span> to cancel
        </p>
      </div>

      <template #footer>
        <div class="flex flex-1 items-center justify-end gap-2">
          <Button variant="ghost" size="sm" @click="closeCreate">Cancel</Button>
          <Button size="sm" :disabled="!draftTitle.trim() && !draftContent.trim()" @click="handleAdd">
            <Plus class="h-3.5 w-3.5" :stroke-width="1.75" />
            Create note
          </Button>
        </div>
      </template>
    </Drawer>

    <!-- Detail drawer -->
    <Drawer
      :open="!!detailNote"
      side="right"
      size="lg"
      @close="closeDetail"
    >
      <template #header>
        <StickyNote class="h-4 w-4 text-muted-foreground shrink-0" :stroke-width="1.75" />
        <h3 class="text-sm font-semibold flex-1 truncate">
          {{ detailNote ? displayTitle(detailNote) : 'Note' }}
        </h3>
      </template>

      <div v-if="detailNote" class="p-5">
        <!-- Edit mode -->
        <template v-if="detailEditing">
          <Input v-model="editTitle" placeholder="Title…" class="mb-2 font-medium" />
          <textarea
            v-model="editContent"
            placeholder="Content (Markdown)…"
            class="w-full min-h-[45vh] resize-y rounded-md border border-border bg-background px-3 py-2 text-sm leading-relaxed font-mono focus:outline-none focus-visible:ring-1 focus-visible:ring-ring"
            @keydown="onDetailEditKeydown"
          />
          <div class="mt-3">
            <label class="block text-[11px] font-medium text-muted-foreground mb-1">Project</label>
            <AppSelect v-model="editProject" :options="projectSelectOptions" placeholder="No project">
              <template #leading>
                <FolderOpen class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.75" />
              </template>
            </AppSelect>
          </div>
          <p class="mt-2 text-[11px] text-muted-foreground">
            Markdown supported · <span class="font-medium">⌘/Ctrl+Enter</span> to save · <span class="font-medium">Esc</span> to cancel
          </p>
        </template>

        <!-- View mode -->
        <template v-else>
          <div
            v-if="detailNote.content"
            class="markdown-output text-sm"
            @click="handleLinkClick"
            v-html="renderText(detailNote.content)"
          />
          <p v-else class="text-sm text-muted-foreground italic">This note is empty.</p>
          <div v-if="projectName(detailNote.project_id)" class="mt-4 flex items-center gap-1.5 text-xs text-muted-foreground border-t border-border/60 pt-3">
            <FolderOpen class="h-3.5 w-3.5" :stroke-width="1.75" />
            <span>{{ projectName(detailNote.project_id) }}</span>
          </div>
        </template>
      </div>

      <template #footer>
        <template v-if="detailEditing">
          <div class="flex flex-1 items-center justify-end gap-2">
            <Button variant="ghost" size="sm" @click="cancelDetailEdit">Cancel</Button>
            <Button size="sm" :disabled="!editTitle.trim() && !editContent.trim()" @click="commitDetailEdit">Save</Button>
          </div>
        </template>
        <template v-else>
          <Button variant="destructive" size="sm" @click="deleteFromDetail">
            <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
            Delete
          </Button>
          <div class="flex-1" />
          <Button size="sm" @click="startDetailEdit">
            <Pencil class="h-3.5 w-3.5" :stroke-width="1.75" />
            Edit
          </Button>
        </template>
      </template>
    </Drawer>
  </div>
</template>
