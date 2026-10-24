<script setup lang="ts">
// One node in the file tree (recursive). Folders lazy-load + expand their
// children on click; files emit `open-file`. Indentation scales with `depth`.
// Supports inline rename (driven by the fileTree store) and drag-and-drop move
// (drag an entry onto a folder to move it there — see lib/fileTreeDnd).
import { computed, nextTick, ref, watch } from 'vue'
import { ChevronRight, ChevronDown, Folder, FolderOpen, File, Loader2 } from 'lucide-vue-next'
import { useFileTreeStore } from '@/stores/fileTree'
import { useToast } from '@/composables/useToast'
import type { DirEntry } from '@/stores/runs'
import { dropHover, outboundDrag, parentDir, startTreeDrag, treeDrag } from '@/lib/fileTreeDnd'
import { invoke } from '@/lib/tauri'

const props = defineProps<{
  projectPath: string
  entry: DirEntry
  depth: number
  activePath?: string | null
  /** Entry the right-click menu is open on — highlighted so it's clear what the menu acts on. */
  menuPath?: string | null
}>()

const emit = defineEmits<{
  'open-file': [path: string]
  'context-menu': [payload: { entry: DirEntry; x: number; y: number }]
  /** Hover in/out for the panel-level tooltip (`null` = left the row). */
  hover: [payload: { entry: DirEntry; rect: DOMRect } | null]
}>()

const store = useFileTreeStore()
const { toast } = useToast()
const tree = computed(() => store.treeFor(props.projectPath))

const expanded = computed(() => tree.value.expanded.has(props.entry.path))
const loading = computed(() => tree.value.loading.has(props.entry.path))
const children = computed(() => tree.value.children[props.entry.path] ?? [])
const isActive = computed(() => !props.entry.is_dir && props.entry.path === props.activePath)
const isMenuTarget = computed(() => props.entry.path === props.menuPath)

// Indent by depth; keep the first level flush-ish with the chevron column.
const indent = computed(() => `${props.depth * 12 + 6}px`)

function onClick() {
  if (isEditing.value) return
  if (swallowClick) { swallowClick = false; return }
  if (props.entry.is_dir) store.toggle(props.projectPath, props.entry.path)
  else emit('open-file', props.entry.path)
}

function onContextMenu(e: MouseEvent) {
  emit('hover', null)
  emit('context-menu', { entry: props.entry, x: e.clientX, y: e.clientY })
}

// ── Hover tooltip ────────────────────────────────────────────────────────────
// Long names are truncated by the panel width, so the row reports its geometry
// upward and FileTree renders a single floating tooltip with the full name. The
// native `title` attribute can't be used here: WebKit suppresses it on
// draggable elements, which every row is.
function onEnter(e: MouseEvent) {
  if (isEditing.value) return
  emit('hover', { entry: props.entry, rect: (e.currentTarget as HTMLElement).getBoundingClientRect() })
}
function onLeave() {
  emit('hover', null)
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
// Pointer-driven (HTML5 DnD never reaches the DOM under Tauri — see
// lib/fileTreeDnd). The row exposes the folder it drops into via data-drop-dir.
const dropDir = computed(() => (props.entry.is_dir ? props.entry.path : parentDir(props.entry.path)))
const isDropTarget = computed(() =>
  props.entry.is_dir
  && dropHover.value?.projectPath === props.projectPath
  && dropHover.value.dir === props.entry.path,
)
const isDragSource = computed(() =>
  treeDrag.value?.projectPath === props.projectPath && treeDrag.value.path === props.entry.path,
)
let swallowClick = false

function onPointerDown(e: PointerEvent) {
  if (e.button !== 0 || isEditing.value) return
  swallowClick = false
  const drag = startTreeDrag(
    e,
    { projectPath: props.projectPath, path: props.entry.path, name: props.entry.name, isDir: props.entry.is_dir },
    async (dir) => {
      try {
        await store.moveInto(props.projectPath, props.entry.path, dir)
      } catch (err) {
        toast.error(String(err))
      }
    },
    () => void dragOut(),
  )
  // The click that follows a real drag must not open/toggle the row.
  window.addEventListener('pointerup', () => { swallowClick = drag.dragged() }, { once: true, capture: true })
  emit('hover', null)
}

// Dragged past the window edge → continue as a native OS drag (copy) so the
// entry can be dropped into Finder, the Desktop or another app.
async function dragOut() {
  outboundDrag.value = true
  try {
    await invoke('start_file_drag', { projectPath: props.projectPath, relPaths: [props.entry.path] })
  } catch (err) {
    toast.error(String(err))
  } finally {
    outboundDrag.value = false
  }
}
</script>

<template>
  <div
    class="group flex items-center gap-1 py-0.5 pr-2 text-xs rounded cursor-pointer select-none"
    :class="[
      isMenuTarget
        ? 'bg-accent text-foreground font-semibold ring-1 ring-inset ring-primary/60'
        : isActive ? 'bg-primary/15 text-foreground' : 'text-muted-foreground hover:bg-accent/60 hover:text-foreground',
      isDropTarget && 'ring-1 ring-primary/70 bg-primary/10',
      isDragSource && 'opacity-40',
      entry.ignored && !isActive && !isMenuTarget && 'opacity-50',
    ]"
    :style="{ paddingLeft: indent }"
    :data-drop-dir="dropDir"
    @click="onClick"
    @contextmenu.prevent.stop="onContextMenu"
    @mouseenter="onEnter"
    @mouseleave="onLeave"
    @pointerdown="onPointerDown"
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
      :menu-path="menuPath"
      @open-file="(p) => emit('open-file', p)"
      @context-menu="(payload) => emit('context-menu', payload)"
      @hover="(payload) => emit('hover', payload)"
    />
  </template>
</template>
