<script setup lang="ts">
// One node in the file tree (recursive). Folders lazy-load + expand their
// children on click; files emit `open-file`. Indentation scales with `depth`.
// Supports inline rename (driven by the fileTree store) and drag-and-drop move
// (drop an entry onto a folder to move it there).
import { computed, nextTick, ref, watch } from 'vue'
import { ChevronRight, ChevronDown, Folder, FolderOpen, File, Loader2 } from 'lucide-vue-next'
import { useFileTreeStore } from '@/stores/fileTree'
import { useToast } from '@/composables/useToast'
import type { DirEntry } from '@/stores/runs'

const DND_MIME = 'application/x-devdy-path'

const props = defineProps<{
  projectPath: string
  entry: DirEntry
  depth: number
  activePath?: string | null
}>()

const emit = defineEmits<{
  'open-file': [path: string]
  'context-menu': [payload: { entry: DirEntry; x: number; y: number }]
}>()

const store = useFileTreeStore()
const { toast } = useToast()
const tree = computed(() => store.treeFor(props.projectPath))

const expanded = computed(() => tree.value.expanded.has(props.entry.path))
const loading = computed(() => tree.value.loading.has(props.entry.path))
const children = computed(() => tree.value.children[props.entry.path] ?? [])
const isActive = computed(() => !props.entry.is_dir && props.entry.path === props.activePath)

// Indent by depth; keep the first level flush-ish with the chevron column.
const indent = computed(() => `${props.depth * 12 + 6}px`)

function onClick() {
  if (isEditing.value) return
  if (props.entry.is_dir) store.toggle(props.projectPath, props.entry.path)
  else emit('open-file', props.entry.path)
}

function onContextMenu(e: MouseEvent) {
  emit('context-menu', { entry: props.entry, x: e.clientX, y: e.clientY })
}

// ── Inline rename ────────────────────────────────────────────────────────────
const isEditing = computed(() => store.renaming[props.projectPath] === props.entry.path)
const draft = ref(props.entry.name)
const inputEl = ref<HTMLInputElement | null>(null)
let finished = false

watch(isEditing, async (editing) => {
  if (!editing) return
  finished = false
  draft.value = props.entry.name
  await nextTick()
  inputEl.value?.focus()
  // Select the name portion (excluding extension) like VSCode.
  const dot = props.entry.name.lastIndexOf('.')
  if (!props.entry.is_dir && dot > 0) inputEl.value?.setSelectionRange(0, dot)
  else inputEl.value?.select()
})

async function finish(commit: boolean) {
  if (finished) return
  finished = true
  try {
    if (commit) await store.commitRename(props.projectPath, props.entry.path, draft.value)
    else store.cancelRename(props.projectPath)
  } catch (e) {
    store.cancelRename(props.projectPath)
    toast.error(String(e))
  }
}

// ── Drag & drop (move) ───────────────────────────────────────────────────────
const isDropTarget = ref(false)

function onDragStart(e: DragEvent) {
  if (isEditing.value) { e.preventDefault(); return }
  e.dataTransfer?.setData(DND_MIME, props.entry.path)
  if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move'
}
function onDragOver(e: DragEvent) {
  if (!props.entry.is_dir) return
  if (!e.dataTransfer?.types.includes(DND_MIME)) return
  e.preventDefault()
  e.dataTransfer.dropEffect = 'move'
  isDropTarget.value = true
}
function onDragLeave() {
  isDropTarget.value = false
}
async function onDrop(e: DragEvent) {
  isDropTarget.value = false
  if (!props.entry.is_dir) return
  const src = e.dataTransfer?.getData(DND_MIME)
  if (!src || src === props.entry.path) return
  e.preventDefault()
  e.stopPropagation()
  try {
    await store.moveInto(props.projectPath, src, props.entry.path)
  } catch (err) {
    toast.error(String(err))
  }
}
</script>

<template>
  <div
    class="group flex items-center gap-1 py-0.5 pr-2 text-xs rounded cursor-pointer select-none"
    :class="[
      isActive ? 'bg-primary/15 text-foreground' : 'text-muted-foreground hover:bg-accent/60 hover:text-foreground',
      isDropTarget && 'ring-1 ring-primary/70 bg-primary/10',
    ]"
    :style="{ paddingLeft: indent }"
    :title="entry.name"
    :draggable="!isEditing"
    @click="onClick"
    @contextmenu.prevent.stop="onContextMenu"
    @dragstart="onDragStart"
    @dragover="onDragOver"
    @dragleave="onDragLeave"
    @drop="onDrop"
  >
    <!-- Chevron column (only for directories) -->
    <span class="w-3.5 shrink-0 flex items-center justify-center">
      <Loader2 v-if="loading" class="h-3 w-3 animate-spin" />
      <template v-else-if="entry.is_dir">
        <ChevronDown v-if="expanded" class="h-3.5 w-3.5" :stroke-width="2" />
        <ChevronRight v-else class="h-3.5 w-3.5" :stroke-width="2" />
      </template>
    </span>

    <!-- Type icon -->
    <FolderOpen v-if="entry.is_dir && expanded" class="h-3.5 w-3.5 shrink-0 text-primary/80" :stroke-width="2" />
    <Folder v-else-if="entry.is_dir" class="h-3.5 w-3.5 shrink-0 text-primary/80" :stroke-width="2" />
    <File v-else class="h-3.5 w-3.5 shrink-0 opacity-70" :stroke-width="2" />

    <!-- Name (or inline rename input) -->
    <input
      v-if="isEditing"
      ref="inputEl"
      v-model="draft"
      class="flex-1 min-w-0 bg-background border border-primary/60 rounded px-1 py-0 text-xs text-foreground outline-none focus:ring-1 focus:ring-ring"
      @click.stop
      @keydown.enter.prevent="finish(true)"
      @keydown.esc.prevent="finish(false)"
      @blur="finish(true)"
    />
    <span v-else class="truncate">{{ entry.name }}</span>
  </div>

  <!-- Children (recursive) -->
  <template v-if="entry.is_dir && expanded">
    <div v-if="!loading && children.length === 0" class="py-0.5 text-[11px] text-muted-foreground/60" :style="{ paddingLeft: `${(depth + 1) * 12 + 22}px` }">
      empty
    </div>
    <FileTreeNode
      v-for="child in children"
      :key="child.path"
      :project-path="projectPath"
      :entry="child"
      :depth="depth + 1"
      :active-path="activePath"
      @open-file="(p) => emit('open-file', p)"
      @context-menu="(payload) => emit('context-menu', payload)"
    />
  </template>
</template>
