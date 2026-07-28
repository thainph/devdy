<script setup lang="ts">
// One node in the file tree (recursive). Folders lazy-load + expand their
// children on click; files emit `open-file`. Indentation scales with `depth`.
import { computed } from 'vue'
import { ChevronRight, ChevronDown, Folder, FolderOpen, File, Loader2 } from 'lucide-vue-next'
import { useFileTreeStore } from '@/stores/fileTree'
import type { DirEntry } from '@/stores/runs'

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
const tree = computed(() => store.treeFor(props.projectPath))

const expanded = computed(() => tree.value.expanded.has(props.entry.path))
const loading = computed(() => tree.value.loading.has(props.entry.path))
const children = computed(() => tree.value.children[props.entry.path] ?? [])
const isActive = computed(() => !props.entry.is_dir && props.entry.path === props.activePath)

// Indent by depth; keep the first level flush-ish with the chevron column.
const indent = computed(() => `${props.depth * 12 + 6}px`)

function onClick() {
  if (props.entry.is_dir) store.toggle(props.projectPath, props.entry.path)
  else emit('open-file', props.entry.path)
}

function onContextMenu(e: MouseEvent) {
  emit('context-menu', { entry: props.entry, x: e.clientX, y: e.clientY })
}
</script>

<template>
  <div
    class="group flex items-center gap-1 py-0.5 pr-2 text-xs rounded cursor-pointer select-none"
    :class="isActive ? 'bg-primary/15 text-foreground' : 'text-muted-foreground hover:bg-accent/60 hover:text-foreground'"
    :style="{ paddingLeft: indent }"
    :title="entry.name"
    @click="onClick"
    @contextmenu.prevent.stop="onContextMenu"
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

    <span class="truncate">{{ entry.name }}</span>
  </div>

  <!-- Children (recursive) -->
  <template v-if="entry.is_dir && expanded">
    <div v-if="!loading && children.length === 0" class="py-0.5 text-[11px] text-muted-foreground/60" :style="{ paddingLeft: `${(depth + 1) * 12 + 22}px` }">
      trống
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
