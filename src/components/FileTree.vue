<script setup lang="ts">
// VSCode-style file explorer panel for a project. Loads the root level on mount
// (and whenever the project changes), then lazy-loads deeper levels as folders
// are expanded. Clicking a file bubbles `open-file` so the host (RunView) can
// open it in its own window. Right-clicking an entry (or empty space) opens the
// shared <ContextMenu> filled with <FileActionsMenu> — the same rows the file
// window's ⋯ menu shows. Only "new file / new folder" is added here, because
// only the tree knows which folder to create in.
import { computed, ref, watch, onBeforeUnmount } from 'vue'
import { useI18n } from 'vue-i18n'
import { RotateCw, ListCollapse } from 'lucide-vue-next'
import FileTreeNode from '@/components/FileTreeNode.vue'
import FileActionsMenu from '@/components/FileActionsMenu.vue'
import { ContextMenu } from '@/components/ui'
import type { FileTarget } from '@/lib/fileActions'
import { useFileTreeStore } from '@/stores/fileTree'
import { useToast } from '@/composables/useToast'
import { usePrompt } from '@/composables/usePrompt'
import { invoke } from '@/lib/tauri'
import type { DirEntry } from '@/stores/runs'

const DND_MIME = 'application/x-devdy-path'

const props = defineProps<{
  projectPath: string
  activePath?: string | null
  /** Whether the Files panel is currently visible. The live FS watcher only runs
   *  while true, so a hidden tree costs nothing. Defaults to on when omitted. */
  active?: boolean
}>()

const emit = defineEmits<{
  'open-file': [path: string]
}>()

const { t } = useI18n()
const store = useFileTreeStore()
const { toast } = useToast()
const { prompt } = usePrompt()
const tree = computed(() => store.treeFor(props.projectPath))
const rootEntries = computed(() => tree.value.children[''] ?? [])
const rootLoading = computed(() => tree.value.loading.has(''))
const rootError = computed(() => tree.value.error[''])
// Reload spins until every expanded level has been re-fetched, not just the root.
const reloading = computed(() => rootLoading.value || store.refreshing[props.projectPath] === true)

watch(
  () => props.projectPath,
  (p) => { if (p) store.loadDir(p, '') },
  { immediate: true },
)

// ── Live FS watch lifecycle ─────────────────────────────────────────────────
// Watch only the dirs currently shown: root plus every expanded folder whose
// children are loaded (a collapsed/unloaded folder has nothing visible to keep
// fresh). Non-reactive `node_modules` etc. stay unwatched unless the user opens
// them. The set feeds the backend, which watches each dir non-recursively.
const watchedDirs = computed(() => {
  const t = tree.value
  const dirs = new Set<string>([''])
  for (const d of t.expanded) if (t.children[d]) dirs.add(d)
  return [...dirs].sort()
})

// Sync the backend watcher to (active tab, project, visible dirs). Tracks the
// project currently being watched so a project switch or hide tears the old one
// down before starting the new — otherwise a stale watcher would linger.
let watchedProject: string | null = null
async function syncWatch() {
  const p = props.projectPath
  const on = props.active !== false
  if (watchedProject && (watchedProject !== p || !on)) {
    const prev = watchedProject
    watchedProject = null
    await invoke('stop_file_tree_watch', { projectPath: prev }).catch(() => {})
  }
  if (!on || !p) return
  watchedProject = p
  await invoke('set_file_tree_watch', { projectPath: p, relDirs: watchedDirs.value }).catch(() => {})
}
watch([() => props.active, () => props.projectPath, watchedDirs], syncWatch, { immediate: true })

function parentDir(relPath: string): string {
  const i = relPath.lastIndexOf('/')
  return i === -1 ? '' : relPath.slice(0, i)
}

// ── Hover tooltip ───────────────────────────────────────────────────────────
// One floating tooltip for the whole panel (rows only report hover + geometry),
// showing the full name and relative path that the narrow column truncates.
const TIP_DELAY_MS = 350
const TIP_MAX_W = 360
const tip = ref<{ name: string; path: string; ignored: boolean; x: number; y: number; above: boolean } | null>(null)
let tipTimer: ReturnType<typeof setTimeout> | null = null

function hideTip() {
  if (tipTimer) { clearTimeout(tipTimer); tipTimer = null }
  tip.value = null
}

function onHover(payload: { entry: DirEntry; rect: DOMRect } | null) {
  if (tipTimer) { clearTimeout(tipTimer); tipTimer = null }
  if (!payload) { tip.value = null; return }
  const { entry, rect } = payload
  tipTimer = setTimeout(() => {
    tipTimer = null
    // Anchor under the row, flipping above it near the bottom edge, and keep the
    // box inside the viewport horizontally.
    const above = rect.bottom + 76 > window.innerHeight
    tip.value = {
      name: entry.name,
      path: entry.path,
      ignored: entry.ignored,
      x: Math.max(8, Math.min(rect.left + 12, window.innerWidth - TIP_MAX_W - 8)),
      y: above ? rect.top - 6 : rect.bottom + 6,
      above,
    }
  }, TIP_DELAY_MS)
}

// ── Context menu ────────────────────────────────────────────────────────────
const menu = ref<{ entry: DirEntry | null; x: number; y: number } | null>(null)
// What the shared menu acts on; null on an empty-space click (project root).
const menuTarget = computed<FileTarget | null>(() => {
  const e = menu.value?.entry
  return e ? { path: e.path, name: e.name, isDir: e.is_dir } : null
})

function openMenu(payload: { entry: DirEntry | null; x: number; y: number }) {
  hideTip()
  // ContextMenu clamps itself to the viewport from its measured size.
  menu.value = { ...payload }
}
function openRootMenu(e: MouseEvent) {
  openMenu({ entry: null, x: e.clientX, y: e.clientY })
}
function closeMenu() { menu.value = null }

// Directory a create/paste targets: the folder itself, a file's parent, or root.
function containerDir(): string {
  const e = menu.value?.entry
  if (!e) return ''
  return e.is_dir ? e.path : parentDir(e.path)
}

// ── Create ──────────────────────────────────────────────────────────────────
async function promptCreate(kind: 'file' | 'folder', dir: string) {
  const isFile = kind === 'file'
  const name = await prompt({
    title: isFile ? t('files.tree.newFile') : t('files.tree.newFolder'),
    label: isFile ? t('files.tree.fileNameLabel') : t('files.tree.folderNameLabel'),
    placeholder: isFile ? t('files.tree.fileNamePlaceholder') : t('files.tree.folderNamePlaceholder'),
    confirmLabel: t('files.tree.create'),
  })
  if (!name) return
  try {
    if (isFile) {
      const p = await store.createFile(props.projectPath, dir, name)
      emit('open-file', p)
      toast.success(t('files.tree.toastFileCreated'))
    } else {
      await store.createDir(props.projectPath, dir, name)
      toast.success(t('files.tree.toastFolderCreated'))
    }
  } catch (e) { toast.error(String(e)) }
}
function newFile() { const dir = containerDir(); closeMenu(); promptCreate('file', dir) }
function newFolder() { const dir = containerDir(); closeMenu(); promptCreate('folder', dir) }

// ── Rename (inline) / Delete (to trash) ─────────────────────────────────────
function renameEntry(target: FileTarget) {
  closeMenu()
  store.beginRename(props.projectPath, target.path)
}

// ── Drop onto empty panel area → move to project root ───────────────────────
async function onRootDrop(e: DragEvent) {
  const src = e.dataTransfer?.getData(DND_MIME)
  if (!src || parentDir(src) === '') return
  try { await store.moveInto(props.projectPath, src, '') }
  catch (err) { toast.error(String(err)) }
}

onBeforeUnmount(() => {
  hideTip()
  if (watchedProject) invoke('stop_file_tree_watch', { projectPath: watchedProject }).catch(() => {})
})
</script>

<template>
  <div class="flex flex-col h-full min-h-0">
    <!-- Header -->
    <div class="flex items-center justify-between gap-2 px-3 h-9 shrink-0 border-b border-border/60">
      <span class="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground truncate">{{ t('files.tree.explorer') }}</span>
      <div class="flex items-center gap-0.5 shrink-0">
        <button
          class="p-1 rounded text-muted-foreground hover:text-foreground hover:bg-accent/60 cursor-pointer"
          :title="t('files.tree.newFile')"
          @click="promptCreate('file', '')"
        >
          <FilePlus class="h-3.5 w-3.5" :stroke-width="2" />
        </button>
        <button
          class="p-1 rounded text-muted-foreground hover:text-foreground hover:bg-accent/60 cursor-pointer"
          :title="t('files.tree.newFolder')"
          @click="promptCreate('folder', '')"
        >
          <FolderPlus class="h-3.5 w-3.5" :stroke-width="2" />
        </button>
        <button
          class="p-1 rounded text-muted-foreground hover:text-foreground hover:bg-accent/60 cursor-pointer"
          :title="t('files.tree.collapseAll')"
          @click="store.collapseAll(projectPath)"
        >
          <ListCollapse class="h-3.5 w-3.5" :stroke-width="2" />
        </button>
        <button
          class="p-1 rounded text-muted-foreground hover:text-foreground hover:bg-accent/60 cursor-pointer"
          :title="t('files.tree.reload')"
          :class="reloading && 'opacity-50 pointer-events-none'"
          @click="store.refresh(projectPath)"
        >
          <RotateCw class="h-3.5 w-3.5" :class="reloading && 'animate-spin'" :stroke-width="2" />
        </button>
      </div>
    </div>

    <!-- Body -->
    <div
      class="flex-1 min-h-0 overflow-y-auto py-1"
      @contextmenu.self.prevent="openRootMenu"
      @dragover.prevent
      @drop="onRootDrop"
      @scroll="hideTip"
      @mouseleave="hideTip"
    >
      <div v-if="rootError" class="px-3 py-2 text-xs text-destructive">{{ rootError }}</div>
      <div v-else-if="rootLoading && rootEntries.length === 0" class="px-3 py-2 text-xs text-muted-foreground">{{ t('common.loading') }}</div>
      <div v-else-if="rootEntries.length === 0" class="px-3 py-6 text-xs text-muted-foreground" @contextmenu.prevent="openRootMenu">{{ t('files.tree.emptyFolder') }}</div>
      <FileTreeNode
        v-for="entry in rootEntries"
        :key="entry.path"
        :project-path="projectPath"
        :entry="entry"
        :depth="0"
        :active-path="activePath"
        @open-file="(p) => emit('open-file', p)"
        @context-menu="openMenu"
        @hover="onHover"
      />
    </div>

    <!-- Hover tooltip: full name + relative path for truncated rows. -->
    <Teleport to="body">
      <div
        v-if="tip"
        class="fixed z-[90] pointer-events-none rounded-md border border-border bg-popover px-2 py-1.5 shadow-lg"
        :style="{
          left: `${tip.x}px`,
          top: `${tip.y}px`,
          maxWidth: `${TIP_MAX_W}px`,
          transform: tip.above ? 'translateY(-100%)' : undefined,
        }"
      >
        <div class="text-xs text-foreground break-all">{{ tip.name }}</div>
        <div v-if="tip.path !== tip.name" class="text-[11px] text-muted-foreground break-all">{{ tip.path }}</div>
        <div v-if="tip.ignored" class="text-[11px] text-muted-foreground/80 italic">{{ t('files.tree.gitIgnored') }}</div>
      </div>
    </Teleport>

    <!-- Right-click menu. Same component AND same rows as the file window's ⋯
         menu — only "new file / new folder" is extra here, because only the tree
         knows which folder to create in. -->
    <ContextMenu :open="!!menu" :x="menu?.x ?? 0" :y="menu?.y ?? 0" @close="closeMenu">
      <FileActionsMenu
        :project-path="projectPath"
        :target="menuTarget"
        show-create
        @create-file="newFile"
        @create-folder="newFolder"
        @rename="renameEntry"
      />
    </ContextMenu>
  </div>
</template>
