<script setup lang="ts">
// VSCode-style file explorer panel for a project. Loads the root level on mount
// (and whenever the project changes), then lazy-loads deeper levels as folders
// are expanded. Clicking a file bubbles `open-file` so the host (RunView) can
// show it in the shared <FileViewer>. Right-clicking an entry opens a context
// menu (copy path, mention in composer, open in Chrome).
import { computed, ref, watch, onBeforeUnmount } from 'vue'
import { RotateCw, ListCollapse, Copy, AtSign, Chrome } from 'lucide-vue-next'
import FileTreeNode from '@/components/FileTreeNode.vue'
import { useFileTreeStore } from '@/stores/fileTree'
import { useToast } from '@/composables/useToast'
import { invoke } from '@/lib/tauri'
import type { DirEntry } from '@/stores/runs'

const props = defineProps<{
  projectPath: string
  activePath?: string | null
}>()

const emit = defineEmits<{
  'open-file': [path: string]
  'mention-file': [path: string]
}>()

const store = useFileTreeStore()
const { toast } = useToast()
const tree = computed(() => store.treeFor(props.projectPath))
const rootEntries = computed(() => tree.value.children[''] ?? [])
const rootLoading = computed(() => tree.value.loading.has(''))
const rootError = computed(() => tree.value.error[''])

watch(
  () => props.projectPath,
  (p) => { if (p) store.loadDir(p, '') },
  { immediate: true },
)

// ── Context menu ────────────────────────────────────────────────────────────
const menu = ref<{ entry: DirEntry; x: number; y: number } | null>(null)
const menuEl = ref<HTMLElement | null>(null)

// Absolute path for the currently targeted entry (backend commands + clipboard
// use the real on-disk path, matching the FileViewer's "copy path" behavior).
function absOf(relPath: string): string {
  const root = props.projectPath.replace(/\/+$/, '')
  return `${root}/${relPath}`
}

function openMenu(payload: { entry: DirEntry; x: number; y: number }) {
  // Clamp so the menu never overflows the right / bottom edges.
  const w = 220, h = 120
  const x = Math.min(payload.x, window.innerWidth - w)
  const y = Math.min(payload.y, window.innerHeight - h)
  menu.value = { entry: payload.entry, x, y }
}

function closeMenu() { menu.value = null }

async function copyPath() {
  const entry = menu.value?.entry
  closeMenu()
  if (!entry) return
  try {
    await navigator.clipboard.writeText(absOf(entry.path))
    toast.success('Đã copy đường dẫn')
  } catch { toast.error('Không copy được đường dẫn') }
}

function mentionFile() {
  const entry = menu.value?.entry
  closeMenu()
  if (entry) emit('mention-file', `${entry.path}${entry.is_dir ? '/' : ''}`)
}

async function openInChrome() {
  const entry = menu.value?.entry
  closeMenu()
  if (!entry) return
  try {
    await invoke('open_in_chrome', { path: absOf(entry.path) })
  } catch (e) { toast.error(String(e)) }
}

// Dismiss on outside interaction / escape. Ignore pointerdowns inside the menu
// itself, otherwise this capture-phase handler would close it before the item's
// click fires and the action would never run.
function onGlobalPointer(e: PointerEvent) {
  if (!menu.value) return
  if (menuEl.value && e.target instanceof Node && menuEl.value.contains(e.target)) return
  closeMenu()
}
function onKeydown(e: KeyboardEvent) { if (e.key === 'Escape') closeMenu() }
watch(menu, (m) => {
  if (m) {
    window.addEventListener('pointerdown', onGlobalPointer, true)
    window.addEventListener('keydown', onKeydown, true)
  } else {
    window.removeEventListener('pointerdown', onGlobalPointer, true)
    window.removeEventListener('keydown', onKeydown, true)
  }
})
onBeforeUnmount(() => {
  window.removeEventListener('pointerdown', onGlobalPointer, true)
  window.removeEventListener('keydown', onKeydown, true)
})
</script>

<template>
  <div class="flex flex-col h-full min-h-0">
    <!-- Header -->
    <div class="flex items-center justify-between gap-2 px-3 h-9 shrink-0 border-b border-border/60">
      <span class="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground truncate">Explorer</span>
      <div class="flex items-center gap-0.5 shrink-0">
        <button
          class="p-1 rounded text-muted-foreground hover:text-foreground hover:bg-accent/60 cursor-pointer"
          title="Thu gọn tất cả"
          @click="store.collapseAll(projectPath)"
        >
          <ListCollapse class="h-3.5 w-3.5" :stroke-width="2" />
        </button>
        <button
          class="p-1 rounded text-muted-foreground hover:text-foreground hover:bg-accent/60 cursor-pointer"
          title="Tải lại"
          :class="rootLoading && 'opacity-50 pointer-events-none'"
          @click="store.refresh(projectPath)"
        >
          <RotateCw class="h-3.5 w-3.5" :class="rootLoading && 'animate-spin'" :stroke-width="2" />
        </button>
      </div>
    </div>

    <!-- Body -->
    <div class="flex-1 min-h-0 overflow-y-auto py-1">
      <div v-if="rootError" class="px-3 py-2 text-xs text-destructive">{{ rootError }}</div>
      <div v-else-if="rootLoading && rootEntries.length === 0" class="px-3 py-2 text-xs text-muted-foreground">Đang tải…</div>
      <div v-else-if="rootEntries.length === 0" class="px-3 py-2 text-xs text-muted-foreground">Thư mục trống</div>
      <FileTreeNode
        v-for="entry in rootEntries"
        :key="entry.path"
        :project-path="projectPath"
        :entry="entry"
        :depth="0"
        :active-path="activePath"
        @open-file="(p) => emit('open-file', p)"
        @context-menu="openMenu"
      />
    </div>
  </div>

  <!-- Right-click context menu (teleported so it floats above everything) -->
  <Teleport to="body">
    <div
      v-if="menu"
      ref="menuEl"
      class="fixed z-[100] min-w-[200px] py-1 rounded-md border border-border bg-popover shadow-lg text-xs"
      :style="{ left: `${menu.x}px`, top: `${menu.y}px` }"
      @contextmenu.prevent
    >
      <button class="w-full flex items-center gap-2 px-3 py-1.5 text-foreground hover:bg-accent cursor-pointer" @click="copyPath">
        <Copy class="h-3.5 w-3.5" :stroke-width="2" /> Copy file path
      </button>
      <button class="w-full flex items-center gap-2 px-3 py-1.5 text-foreground hover:bg-accent cursor-pointer" @click="mentionFile">
        <AtSign class="h-3.5 w-3.5" :stroke-width="2" /> Mention trong chatbox
      </button>
      <button class="w-full flex items-center gap-2 px-3 py-1.5 text-foreground hover:bg-accent cursor-pointer" @click="openInChrome">
        <Chrome class="h-3.5 w-3.5" :stroke-width="2" /> Open with Chrome
      </button>
    </div>
  </Teleport>
</template>
