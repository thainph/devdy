<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue'
import { useTodosStore } from '@/stores/todos'
import { Button, Card, Drawer } from '@/components/ui'
import MarkdownPreview from '@/components/MarkdownPreview.vue'
import { useConfirm } from '@/composables/useConfirm'
import { useMarkdown } from '@/lib/markdown'
import { openUrl } from '@tauri-apps/plugin-opener'
import { Plus, GripVertical, Trash2, ListTodo, Check, Pencil } from 'lucide-vue-next'

const store = useTodosStore()
const { confirm } = useConfirm()
const { renderText, loadMarkdown } = useMarkdown()

onMounted(() => {
  store.fetchTodos()
  loadMarkdown()
})

const remaining = computed(() => store.todos.filter(t => !t.done).length)
const hasCompleted = computed(() => store.todos.some(t => t.done))

// --- Create drawer ------------------------------------------------------------
// Creating a task is its own focused surface (a right-side drawer), kept fully
// separate from the list. The list screen only browses; this drawer only adds.
const createOpen = ref(false)
const draft = ref('')
const createRef = ref<HTMLTextAreaElement | null>(null)

function openCreate() {
  draft.value = ''
  createOpen.value = true
  nextTick(() => createRef.value?.focus())
}

function closeCreate() {
  createOpen.value = false
}

async function handleAdd() {
  const text = draft.value.trim()
  if (!text) return
  await store.add(text)
  closeCreate()
}

// ⌘/Ctrl+Enter saves; Esc cancels. Plain Enter inserts a newline (markdown /
// multi-line tasks are common).
function onCreateKeydown(e: KeyboardEvent) {
  if (e.key === 'Enter' && (e.metaKey || e.ctrlKey) && !e.isComposing) {
    e.preventDefault()
    handleAdd()
  } else if (e.key === 'Escape') {
    e.preventDefault()
    closeCreate()
  }
}

// Markdown links must not navigate the in-app webview (it would open a blank
// window / hijack the SPA). Intercept anchor clicks and hand http/mailto URLs to
// the OS default browser via the opener plugin. Returns true when handled.
function handleLinkClick(e: MouseEvent): boolean {
  const anchor = (e.target as HTMLElement).closest('a') as HTMLAnchorElement | null
  if (!anchor) return false
  e.preventDefault()
  const href = anchor.getAttribute('href') ?? ''
  if (/^(https?:|mailto:)/i.test(href)) openUrl(href).catch(() => { /* opener unavailable */ })
  return true
}

// List row: a link opens externally; anything else opens the detail drawer.
function onContentClick(id: string, e: MouseEvent) {
  if (handleLinkClick(e)) {
    e.stopPropagation()
    return
  }
  openDetail(id)
}

// --- Detail drawer ------------------------------------------------------------
// Clicking a task opens a left-side drawer showing the full markdown content,
// with an inline editor for long tasks.
const detailId = ref<string | null>(null)
const detailTodo = computed(() => store.todos.find(t => t.id === detailId.value) ?? null)
const detailEditing = ref(false)
const detailText = ref('')

function openDetail(id: string) {
  detailId.value = id
  detailEditing.value = false
}

function closeDetail() {
  detailId.value = null
  detailEditing.value = false
  detailText.value = ''
}

function startDetailEdit() {
  if (!detailTodo.value) return
  detailText.value = detailTodo.value.text
  detailEditing.value = true
}

function commitDetailEdit() {
  if (!detailId.value) return
  const trimmed = detailText.value.trim()
  if (!trimmed) return // empty save is a no-op; use Delete to remove
  store.update(detailId.value, trimmed)
  detailEditing.value = false
}

function cancelDetailEdit() {
  detailEditing.value = false
  detailText.value = ''
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
  if (!detailTodo.value) return
  if (!(await confirm({
    title: 'Delete task',
    message: 'Remove this task?',
    confirmLabel: 'Delete',
  }))) return
  const id = detailTodo.value.id
  closeDetail()
  store.remove(id)
}

// --- Drag & drop reorder (pointer-based) --------------------------------------
// Tauri's webview reserves native HTML5 drag-and-drop for OS file drops (used by
// the run composer's file attach), which swallows dragstart/dragover — so
// reordering is implemented with raw pointer events instead. Only the grip
// handle starts a drag.
const dragIndex = ref<number | null>(null)
const dragOverIndex = ref<number | null>(null)

function indexFromPoint(x: number, y: number): number | null {
  const row = document.elementFromPoint(x, y)?.closest('[data-todo-index]') as HTMLElement | null
  if (!row) return null
  const idx = Number(row.dataset.todoIndex)
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

async function handleClearCompleted() {
  const count = store.todos.filter(t => t.done).length
  if (count === 0) return
  if (!(await confirm({
    title: 'Clear completed',
    message: `Remove ${count} completed task${count === 1 ? '' : 's'}?`,
    confirmLabel: 'Clear',
  }))) return
  store.clearCompleted()
}
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- Page header -->
    <div class="flex items-center justify-between px-6 h-13 border-b border-border/60 shrink-0">
      <div class="flex items-center gap-2">
        <h1 class="text-sm font-semibold">Todos</h1>
        <span
          v-if="store.todos.length > 0"
          class="flex h-4 min-w-4 items-center justify-center rounded-full bg-muted px-1.5 text-[10px] font-medium text-muted-foreground"
        >
          {{ remaining }}
        </span>
      </div>
      <div class="flex items-center gap-2">
        <Button
          v-if="hasCompleted"
          variant="outline"
          @click="handleClearCompleted"
        >
          <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
          Clear completed
        </Button>
        <Button @click="openCreate">
          <Plus class="h-3.5 w-3.5" :stroke-width="2" />
          New task
        </Button>
      </div>
    </div>

    <!-- Content -->
    <div class="flex-1 overflow-auto p-6">
      <div class="w-full">
        <!-- Loading skeleton -->
        <div v-if="store.loading && store.todos.length === 0" class="space-y-2">
          <div v-for="i in 4" :key="i" class="h-11 rounded-lg border border-border bg-card animate-pulse" />
        </div>

        <!-- Error -->
        <div v-else-if="store.error" class="p-4 bg-destructive/10 text-destructive rounded-lg text-sm border border-destructive/20">
          {{ store.error }}
        </div>

        <!-- Empty state -->
        <div
          v-else-if="store.todos.length === 0"
          class="flex flex-col items-center justify-center text-center min-h-72"
        >
          <div class="flex h-12 w-12 items-center justify-center rounded-xl bg-muted mb-4">
            <ListTodo class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
          </div>
          <p class="text-sm font-medium">No tasks yet</p>
          <p class="text-xs text-muted-foreground mt-1 mb-4 max-w-56">Create your first task to track what needs doing. Drag to reorder by priority.</p>
          <Button size="md" @click="openCreate">
            <Plus class="h-3.5 w-3.5" :stroke-width="2" />
            Create task
          </Button>
        </div>

        <!-- List -->
        <Card v-else body-class="divide-y divide-border/50">
          <div
            v-for="(todo, index) in store.todos"
            :key="todo.id"
            :data-todo-index="index"
            class="group flex items-start gap-2.5 px-3 py-2.5 transition-colors"
            :class="[
              dragOverIndex === index && dragIndex !== index ? 'bg-primary/5' : '',
              dragIndex === index ? 'opacity-40' : '',
            ]"
          >
            <!-- Drag handle -->
            <span
              class="mt-0.5 shrink-0 cursor-grab text-muted-foreground/40 transition-colors group-hover:text-muted-foreground active:cursor-grabbing touch-none"
              title="Drag to reorder"
              @pointerdown="startDrag(index, $event)"
            >
              <GripVertical class="h-4 w-4" :stroke-width="1.75" />
            </span>

            <!-- Checkbox -->
            <button
              type="button"
              class="mt-0.5 flex h-4.5 w-4.5 shrink-0 items-center justify-center rounded border transition-colors cursor-pointer"
              :class="todo.done
                ? 'bg-primary border-primary text-primary-foreground'
                : 'border-border hover:border-primary/60'"
              :title="todo.done ? 'Mark as not done' : 'Mark as done'"
              @click="store.toggle(todo.id)"
            >
              <Check v-if="todo.done" class="h-3 w-3" :stroke-width="3" />
            </button>

            <!-- Content (click to open detail) -->
            <div
              class="flex-1 min-w-0 cursor-pointer"
              :class="todo.done ? 'opacity-55 line-through decoration-muted-foreground/60' : ''"
              title="Click to open"
              @click="onContentClick(todo.id, $event)"
            >
              <MarkdownPreview :html="renderText(todo.text)" />
            </div>
          </div>
        </Card>
      </div>
    </div>

    <!-- Create drawer (separate surface: only for adding new tasks) -->
    <Drawer
      :open="createOpen"
      side="right"
      size="lg"
      @close="closeCreate"
    >
      <template #header>
        <Plus class="h-4 w-4 text-muted-foreground shrink-0" :stroke-width="1.75" />
        <h3 class="text-sm font-semibold flex-1 truncate">New task</h3>
      </template>

      <div class="p-5">
        <label class="block text-[11px] font-medium text-muted-foreground mb-1">Task</label>
        <textarea
          ref="createRef"
          v-model="draft"
          placeholder="What needs doing? (Markdown & checklists supported)"
          class="w-full min-h-[45vh] resize-y rounded-md border border-border bg-background px-3 py-2 text-sm leading-relaxed font-mono focus:outline-none focus-visible:ring-1 focus-visible:ring-ring"
          @keydown="onCreateKeydown"
        />
        <p class="mt-3 text-[11px] text-muted-foreground">
          Markdown supported · <span class="font-medium">⌘/Ctrl+Enter</span> to save · <span class="font-medium">Esc</span> to cancel
        </p>
      </div>

      <template #footer>
        <div class="flex flex-1 items-center justify-end gap-2">
          <Button variant="ghost" size="sm" @click="closeCreate">Cancel</Button>
          <Button size="sm" :disabled="!draft.trim()" @click="handleAdd">
            <Plus class="h-3.5 w-3.5" :stroke-width="1.75" />
            Create task
          </Button>
        </div>
      </template>
    </Drawer>

    <!-- Detail drawer (slides in from the left) -->
    <Drawer
      :open="!!detailTodo"
      side="right"
      size="lg"
      @close="closeDetail"
    >
      <template #header>
        <button
          type="button"
          class="flex h-4.5 w-4.5 shrink-0 items-center justify-center rounded border transition-colors cursor-pointer"
          :class="detailTodo?.done
            ? 'bg-primary border-primary text-primary-foreground'
            : 'border-border hover:border-primary/60'"
          :title="detailTodo?.done ? 'Mark as not done' : 'Mark as done'"
          @click="detailTodo && store.toggle(detailTodo.id)"
        >
          <Check v-if="detailTodo?.done" class="h-3 w-3" :stroke-width="3" />
        </button>
        <h3 class="text-sm font-semibold flex-1 truncate">Task detail</h3>
      </template>

      <div v-if="detailTodo" class="p-5">
        <!-- Edit mode -->
        <template v-if="detailEditing">
          <textarea
            v-model="detailText"
            class="w-full min-h-[55vh] resize-y rounded-md border border-border bg-background px-3 py-2 text-sm leading-relaxed font-mono focus:outline-none focus-visible:ring-1 focus-visible:ring-ring"
            @keydown="onDetailEditKeydown"
          />
          <p class="mt-1.5 text-[11px] text-muted-foreground">
            Markdown supported · <span class="font-medium">⌘/Ctrl+Enter</span> to save · <span class="font-medium">Esc</span> to cancel
          </p>
        </template>
        <!-- View mode -->
        <div
          v-else
          class="markdown-output text-sm"
          :class="detailTodo.done ? 'opacity-60 line-through decoration-muted-foreground/60' : ''"
          @click="handleLinkClick"
          v-html="renderText(detailTodo.text)"
        />
      </div>

      <template #footer>
        <template v-if="detailEditing">
          <div class="flex flex-1 items-center justify-end gap-2">
            <Button variant="ghost" size="sm" @click="cancelDetailEdit">Cancel</Button>
            <Button size="sm" :disabled="!detailText.trim()" @click="commitDetailEdit">Save</Button>
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
