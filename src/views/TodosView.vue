<script setup lang="ts">
import { computed, nextTick, onMounted, ref } from 'vue'
import { useTodosStore } from '@/stores/todos'
import { Button, Card } from '@/components/ui'
import { useConfirm } from '@/composables/useConfirm'
import { Plus, GripVertical, Trash2, ListTodo, Check } from 'lucide-vue-next'

const store = useTodosStore()
const { confirm } = useConfirm()

onMounted(() => store.fetchTodos())

const draft = ref('')
const remaining = computed(() => store.todos.filter(t => !t.done).length)
const hasCompleted = computed(() => store.todos.some(t => t.done))

function handleAdd() {
  const text = draft.value.trim()
  if (!text) return
  store.add(text)
  draft.value = ''
}

// --- Inline edit --------------------------------------------------------------
const editingId = ref<string | null>(null)
const editText = ref('')
const editInput = ref<HTMLInputElement | null>(null)

async function startEdit(id: string, current: string) {
  editingId.value = id
  editText.value = current
  await nextTick()
  editInput.value?.focus()
  editInput.value?.select()
}

function commitEdit() {
  if (editingId.value === null) return
  store.update(editingId.value, editText.value)
  editingId.value = null
  editText.value = ''
}

function cancelEdit() {
  editingId.value = null
  editText.value = ''
}

// --- Drag & drop reorder ------------------------------------------------------
const dragIndex = ref<number | null>(null)
const dragOverIndex = ref<number | null>(null)

function onDragStart(index: number, e: DragEvent) {
  dragIndex.value = index
  if (e.dataTransfer) {
    e.dataTransfer.effectAllowed = 'move'
    // Firefox needs data set for the drag to initiate.
    e.dataTransfer.setData('text/plain', String(index))
  }
}

function onDragOver(index: number, e: DragEvent) {
  e.preventDefault()
  if (e.dataTransfer) e.dataTransfer.dropEffect = 'move'
  dragOverIndex.value = index
}

function onDrop(index: number) {
  if (dragIndex.value !== null && dragIndex.value !== index) {
    store.reorder(dragIndex.value, index)
  }
  dragIndex.value = null
  dragOverIndex.value = null
}

function onDragEnd() {
  dragIndex.value = null
  dragOverIndex.value = null
}

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
      <Button
        v-if="hasCompleted"
        variant="outline"
        @click="handleClearCompleted"
      >
        <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
        Clear completed
      </Button>
    </div>

    <!-- Content -->
    <div class="flex-1 overflow-auto p-6">
      <div class="mx-auto w-full max-w-2xl">
        <!-- Quick add -->
        <div class="flex items-center gap-2 mb-4">
          <div class="relative flex-1">
            <Plus class="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" :stroke-width="1.75" />
            <input
              v-model="draft"
              type="text"
              placeholder="Add a task and press Enter…"
              class="flex h-9 w-full rounded-md border border-border bg-background pl-9 pr-3 text-sm shadow-sm transition placeholder:text-muted-foreground focus:outline-none focus-visible:ring-1 focus-visible:ring-ring"
              @keydown.enter="handleAdd"
            />
          </div>
          <Button
            size="md"
            :disabled="!draft.trim()"
            @click="handleAdd"
          >
            Add
          </Button>
        </div>

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
          <p class="text-xs text-muted-foreground mt-1 max-w-56">Jot down a quick task above. Drag to reorder by priority.</p>
        </div>

        <!-- List -->
        <Card v-else body-class="divide-y divide-border/50">
          <div
            v-for="(todo, index) in store.todos"
            :key="todo.id"
            class="group flex items-center gap-2.5 px-3 py-2.5 transition-colors"
            :class="[
              dragOverIndex === index && dragIndex !== index ? 'bg-primary/5' : '',
              dragIndex === index ? 'opacity-40' : '',
            ]"
            draggable="true"
            @dragstart="onDragStart(index, $event)"
            @dragover="onDragOver(index, $event)"
            @drop="onDrop(index)"
            @dragend="onDragEnd"
          >
            <!-- Drag handle -->
            <span
              class="shrink-0 cursor-grab text-muted-foreground/40 transition-colors group-hover:text-muted-foreground active:cursor-grabbing"
              title="Drag to reorder"
            >
              <GripVertical class="h-4 w-4" :stroke-width="1.75" />
            </span>

            <!-- Checkbox -->
            <button
              type="button"
              class="flex h-4.5 w-4.5 shrink-0 items-center justify-center rounded border transition-colors cursor-pointer"
              :class="todo.done
                ? 'bg-primary border-primary text-primary-foreground'
                : 'border-border hover:border-primary/60'"
              :title="todo.done ? 'Mark as not done' : 'Mark as done'"
              @click="store.toggle(todo.id)"
            >
              <Check v-if="todo.done" class="h-3 w-3" :stroke-width="3" />
            </button>

            <!-- Text / inline edit -->
            <input
              v-if="editingId === todo.id"
              ref="editInput"
              v-model="editText"
              type="text"
              class="flex-1 min-w-0 rounded border border-border bg-background px-2 py-1 text-sm focus:outline-none focus-visible:ring-1 focus-visible:ring-ring"
              @keydown.enter="commitEdit"
              @keydown.esc="cancelEdit"
              @blur="commitEdit"
            />
            <span
              v-else
              class="flex-1 min-w-0 truncate text-sm cursor-text"
              :class="todo.done ? 'text-muted-foreground line-through' : 'text-foreground'"
              title="Double-click to edit"
              @dblclick="startEdit(todo.id, todo.text)"
            >
              {{ todo.text }}
            </span>

            <!-- Delete -->
            <Button
              variant="destructive-ghost"
              size="icon-sm"
              class="shrink-0 opacity-0 group-hover:opacity-100 transition-opacity"
              title="Delete"
              @click="store.remove(todo.id)"
            >
              <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
            </Button>
          </div>
        </Card>
      </div>
    </div>
  </div>
</template>
