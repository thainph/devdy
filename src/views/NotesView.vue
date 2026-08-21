<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useNotesStore } from '@/stores/notes'
import { useProjectsStore } from '@/stores/projects'
import { Button, Card, Drawer, Input, AppSelect } from '@/components/ui'
import MarkdownPreview from '@/components/MarkdownPreview.vue'
import { useConfirm } from '@/composables/useConfirm'
import { useMarkdown } from '@/lib/markdown'
import { openUrl } from '@tauri-apps/plugin-opener'
import { Plus, GripVertical, Trash2, StickyNote, ListChecks, Check, Pencil, Search, X, FolderOpen } from 'lucide-vue-next'

const { t } = useI18n()
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
  { value: PROJECT_NONE, label: t('notes.noProject') },
  ...projectsStore.projects.map(p => ({ value: p.id, label: p.name })),
])

// Options for the filter bar: all / none / every project.
const projectFilterOptions = computed(() => [
  { value: FILTER_ALL, label: t('notes.allProjects') },
  { value: FILTER_NONE, label: t('notes.noProject') },
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
  return firstLine?.trim() || t('notes.untitled')
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
    title: t('notes.confirm.deleteTitle'),
    message: t('notes.confirm.deleteMessage'),
    confirmLabel: t('common.delete'),
  }))) return
  const id = detailNote.value.id
  closeDetail()
  store.remove(id)
}

// --- Bulk selection -----------------------------------------------------------
// A dedicated "select mode" turns each card into a multi-select checkbox so
// several notes can be deleted at once. Selection operates on the currently
// visible (filtered) list, and select-all follows the same set.
const selectMode = ref(false)
const selectedIds = ref<Set<string>>(new Set())

const selectedCount = computed(() => selectedIds.value.size)
const allSelected = computed(
  () => filteredNotes.value.length > 0
    && filteredNotes.value.every(n => selectedIds.value.has(n.id)),
)

function enterSelectMode() {
  selectMode.value = true
  selectedIds.value = new Set()
}

function exitSelectMode() {
  selectMode.value = false
  selectedIds.value = new Set()
}

function toggleSelect(id: string) {
  const next = new Set(selectedIds.value)
  if (next.has(id)) next.delete(id)
  else next.add(id)
  selectedIds.value = next
}

function toggleSelectAll() {
  selectedIds.value = allSelected.value
    ? new Set()
    : new Set(filteredNotes.value.map(n => n.id))
}

async function handleDeleteSelected() {
  const count = selectedIds.value.size
  if (count === 0) return
  if (!(await confirm({
    title: t('notes.confirm.bulkDeleteTitle'),
    message: count === 1
      ? t('notes.confirm.bulkDeleteMessageOne', { count })
      : t('notes.confirm.bulkDeleteMessageMany', { count }),
    confirmLabel: t('common.delete'),
  }))) return
  await store.removeMany([...selectedIds.value])
  exitSelectMode()
}

function onCardClickOrSelect(id: string, e: MouseEvent) {
  if (selectMode.value) {
    toggleSelect(id)
    return
  }
  onCardClick(id, e)
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
        <h1 class="text-sm font-semibold">{{ t('notes.title') }}</h1>
        <span
          v-if="store.notes.length > 0"
          class="flex h-4 min-w-4 items-center justify-center rounded-full bg-muted px-1.5 text-[10px] font-medium text-muted-foreground"
        >
          {{ store.notes.length }}
        </span>
      </div>
      <div class="flex items-center gap-2">
        <template v-if="selectMode">
          <span class="text-xs text-muted-foreground tabular-nums">
            {{ t('notes.selectedCount', { count: selectedCount }) }}
          </span>
          <Button variant="outline" @click="toggleSelectAll">
            {{ allSelected ? t('notes.deselectAll') : t('notes.selectAll') }}
          </Button>
          <Button
            variant="destructive"
            :disabled="selectedCount === 0"
            @click="handleDeleteSelected"
          >
            <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('notes.deleteSelected') }}
          </Button>
          <Button variant="ghost" @click="exitSelectMode">
            {{ t('common.cancel') }}
          </Button>
        </template>
        <template v-else>
          <Button
            v-if="store.notes.length > 0"
            variant="outline"
            @click="enterSelectMode"
          >
            <ListChecks class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('notes.select') }}
          </Button>
          <Button @click="openCreate">
            <Plus class="h-3.5 w-3.5" :stroke-width="2" />
            {{ t('notes.newNote') }}
          </Button>
        </template>
      </div>
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
              :placeholder="t('notes.searchPlaceholder')"
              class="pl-8"
            />
            <button
              v-if="search"
              type="button"
              class="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground cursor-pointer"
              :title="t('notes.clearSearch')"
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
          <p class="text-sm font-medium">{{ t('notes.empty.title') }}</p>
          <p class="text-xs text-muted-foreground mt-1 mb-4 max-w-56">{{ t('notes.empty.hint') }}</p>
          <Button size="md" @click="openCreate">
            <Plus class="h-3.5 w-3.5" :stroke-width="2" />
            {{ t('notes.createNote') }}
          </Button>
        </div>

        <!-- No results (filtered) -->
        <div
          v-else-if="filteredNotes.length === 0"
          class="flex flex-col items-center justify-center text-center min-h-40"
        >
          <p class="text-sm font-medium">{{ t('notes.noResults.title') }}</p>
          <button
            type="button"
            class="text-xs text-primary hover:underline mt-1 cursor-pointer"
            @click="clearFilters"
          >
            {{ t('notes.noResults.clearFilters') }}
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
              selectMode && selectedIds.has(note.id) ? 'ring-1 ring-primary/60 border-primary/40' : '',
            ]"
            @click="onCardClickOrSelect(note.id, $event)"
          >
            <div class="flex items-start gap-1.5 p-3">
              <!-- Selection checkbox (select mode) -->
              <button
                v-if="selectMode"
                type="button"
                class="mt-0.5 flex h-4.5 w-4.5 shrink-0 items-center justify-center rounded border transition-colors cursor-pointer"
                :class="selectedIds.has(note.id)
                  ? 'bg-primary border-primary text-primary-foreground'
                  : 'border-border hover:border-primary/60'"
                @click.stop="toggleSelect(note.id)"
              >
                <Check v-if="selectedIds.has(note.id)" class="h-3 w-3" :stroke-width="3" />
              </button>

              <!-- Drag handle (only when not filtering and not selecting) -->
              <span
                v-else-if="!isFiltered"
                class="mt-0.5 shrink-0 cursor-grab text-muted-foreground/30 transition-colors group-hover:text-muted-foreground active:cursor-grabbing touch-none"
                :title="t('notes.dragToReorder')"
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
      :dismiss-on-overlay="false"
      @close="closeCreate"
    >
      <template #header>
        <Plus class="h-4 w-4 text-muted-foreground shrink-0" :stroke-width="1.75" />
        <h3 class="text-sm font-semibold flex-1 truncate">{{ t('notes.newNote') }}</h3>
      </template>

      <div class="p-5">
        <label class="block text-[11px] font-medium text-muted-foreground mb-1">{{ t('notes.form.titleLabel') }}</label>
        <Input
          ref="createTitleRef"
          v-model="draftTitle"
          :placeholder="t('notes.form.titlePlaceholder')"
          class="mb-4 font-medium"
          @keydown="onCreateKeydown"
        />

        <label class="block text-[11px] font-medium text-muted-foreground mb-1">{{ t('notes.form.contentLabel') }}</label>
        <textarea
          v-model="draftContent"
          :placeholder="t('notes.form.contentPlaceholder')"
          class="w-full min-h-[45vh] resize-y rounded-md border border-border bg-background px-3 py-2 text-sm leading-relaxed font-mono focus:outline-none focus-visible:ring-1 focus-visible:ring-ring"
          @keydown="onCreateKeydown"
        />

        <div class="mt-4">
          <label class="block text-[11px] font-medium text-muted-foreground mb-1">{{ t('notes.form.projectLabel') }}</label>
          <AppSelect v-model="draftProject" :options="projectSelectOptions" :placeholder="t('notes.noProject')">
            <template #leading>
              <FolderOpen class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.75" />
            </template>
          </AppSelect>
        </div>

        <p class="mt-3 text-[11px] text-muted-foreground">
          {{ t('notes.form.hint', { save: '⌘/Ctrl+Enter', esc: 'Esc' }) }}
        </p>
      </div>

      <template #footer>
        <div class="flex flex-1 items-center justify-end gap-2">
          <Button variant="ghost" size="sm" @click="closeCreate">{{ t('common.cancel') }}</Button>
          <Button size="sm" :disabled="!draftTitle.trim() && !draftContent.trim()" @click="handleAdd">
            <Plus class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('notes.createNote') }}
          </Button>
        </div>
      </template>
    </Drawer>

    <!-- Detail drawer -->
    <Drawer
      :open="!!detailNote"
      side="right"
      size="lg"
      :dismiss-on-overlay="!detailEditing"
      @close="closeDetail"
    >
      <template #header>
        <StickyNote class="h-4 w-4 text-muted-foreground shrink-0" :stroke-width="1.75" />
        <h3 class="text-sm font-semibold flex-1 truncate">
          {{ detailNote ? displayTitle(detailNote) : t('notes.detail.fallbackTitle') }}
        </h3>
      </template>

      <div v-if="detailNote" class="p-5">
        <!-- Edit mode -->
        <template v-if="detailEditing">
          <Input v-model="editTitle" :placeholder="t('notes.detail.titlePlaceholder')" class="mb-2 font-medium" />
          <textarea
            v-model="editContent"
            :placeholder="t('notes.form.contentPlaceholder')"
            class="w-full min-h-[45vh] resize-y rounded-md border border-border bg-background px-3 py-2 text-sm leading-relaxed font-mono focus:outline-none focus-visible:ring-1 focus-visible:ring-ring"
            @keydown="onDetailEditKeydown"
          />
          <div class="mt-3">
            <label class="block text-[11px] font-medium text-muted-foreground mb-1">{{ t('notes.form.projectLabel') }}</label>
            <AppSelect v-model="editProject" :options="projectSelectOptions" :placeholder="t('notes.noProject')">
              <template #leading>
                <FolderOpen class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.75" />
              </template>
            </AppSelect>
          </div>
          <p class="mt-2 text-[11px] text-muted-foreground">
            {{ t('notes.form.hint', { save: '⌘/Ctrl+Enter', esc: 'Esc' }) }}
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
          <p v-else class="text-sm text-muted-foreground italic">{{ t('notes.detail.empty') }}</p>
          <div v-if="projectName(detailNote.project_id)" class="mt-4 flex items-center gap-1.5 text-xs text-muted-foreground border-t border-border/60 pt-3">
            <FolderOpen class="h-3.5 w-3.5" :stroke-width="1.75" />
            <span>{{ projectName(detailNote.project_id) }}</span>
          </div>
        </template>
      </div>

      <template #footer>
        <template v-if="detailEditing">
          <div class="flex flex-1 items-center justify-end gap-2">
            <Button variant="ghost" size="sm" @click="cancelDetailEdit">{{ t('common.cancel') }}</Button>
            <Button size="sm" :disabled="!editTitle.trim() && !editContent.trim()" @click="commitDetailEdit">{{ t('common.save') }}</Button>
          </div>
        </template>
        <template v-else>
          <Button variant="destructive" size="sm" @click="deleteFromDetail">
            <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('common.delete') }}
          </Button>
          <div class="flex-1" />
          <Button size="sm" @click="startDetailEdit">
            <Pencil class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('common.edit') }}
          </Button>
        </template>
      </template>
    </Drawer>
  </div>
</template>
