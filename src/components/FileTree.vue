<script setup lang="ts">
// VSCode-style file explorer panel for a project. Loads the root level on mount
// (and whenever the project changes), then lazy-loads deeper levels as folders
// are expanded. Clicking a file bubbles `open-file` so the host (RunView) can
// show it in the shared <FileViewer>. Right-clicking an entry (or empty space)
// opens a context menu with the full set of file operations: new file/folder,
// rename, delete (to trash), cut/copy/paste, duplicate, path copy, reveal in
// Finder, open in VSCode/Chrome, and mention in the composer.
import { computed, ref, watch, onBeforeUnmount } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  RotateCw, ListCollapse, FilePlus, FolderPlus, Copy, Link, Scissors,
  ClipboardPaste, CopyPlus, Pencil, Trash2, FolderOpen, Code, Chrome, AtSign,
} from 'lucide-vue-next'
import FileTreeNode from '@/components/FileTreeNode.vue'
import { useFileTreeStore } from '@/stores/fileTree'
import { useToast } from '@/composables/useToast'
import { useConfirm } from '@/composables/useConfirm'
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
  'mention-file': [path: string]
}>()

const { t } = useI18n()
const store = useFileTreeStore()
const { toast } = useToast()
const { confirm } = useConfirm()
const { prompt } = usePrompt()
const tree = computed(() => store.treeFor(props.projectPath))
const rootEntries = computed(() => tree.value.children[''] ?? [])
const rootLoading = computed(() => tree.value.loading.has(''))
const rootError = computed(() => tree.value.error[''])

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

// Internal cut/copy clipboard (in-app only; survives across menu opens).
const clipboard = ref<{ path: string; name: string; mode: 'copy' | 'cut' } | null>(null)

function parentDir(relPath: string): string {
  const i = relPath.lastIndexOf('/')
  return i === -1 ? '' : relPath.slice(0, i)
}

// Absolute path for a relative entry (backend open commands + clipboard use the
// real on-disk path, matching the FileViewer's "copy path" behavior).
function absOf(relPath: string): string {
  const root = props.projectPath.replace(/\/+$/, '')
  return relPath ? `${root}/${relPath}` : root
}

// ── Context menu ────────────────────────────────────────────────────────────
const menu = ref<{ entry: DirEntry | null; x: number; y: number } | null>(null)
const menuEl = ref<HTMLElement | null>(null)
const menuEntry = computed(() => menu.value?.entry ?? null)

function openMenu(payload: { entry: DirEntry | null; x: number; y: number }) {
  // Clamp so the menu never overflows the right / bottom edges.
  const w = 240, h = 440
  const x = Math.min(payload.x, window.innerWidth - w)
  const y = Math.max(8, Math.min(payload.y, window.innerHeight - h))
  menu.value = { entry: payload.entry, x, y }
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
function renameEntry() {
  const e = menu.value?.entry
  closeMenu()
  if (e) store.beginRename(props.projectPath, e.path)
}

async function deleteEntry() {
  const e = menu.value?.entry
  closeMenu()
  if (!e) return
  const ok = await confirm({
    title: e.is_dir ? t('files.tree.deleteFolder') : t('files.tree.deleteFile'),
    message: t('files.tree.moveToTrash', { name: e.name }),
    confirmLabel: t('common.delete'),
    variant: 'destructive',
  })
  if (!ok) return
  try {
    await store.remove(props.projectPath, e.path)
    toast.success(t('files.tree.toastMovedToTrash'))
  } catch (err) { toast.error(String(err)) }
}

// ── Cut / Copy / Paste / Duplicate ──────────────────────────────────────────
function copyToClipboard(mode: 'copy' | 'cut') {
  const e = menu.value?.entry
  closeMenu()
  if (e) clipboard.value = { path: e.path, name: e.name, mode }
}

async function paste() {
  const dir = containerDir()
  const clip = clipboard.value
  closeMenu()
  if (!clip) return
  try {
    if (clip.mode === 'copy') {
      await store.copyInto(props.projectPath, clip.path, dir)
    } else {
      await store.moveInto(props.projectPath, clip.path, dir)
      clipboard.value = null
    }
    toast.success(t('files.tree.toastPasted'))
  } catch (e) { toast.error(String(e)) }
}

async function duplicate() {
  const e = menu.value?.entry
  closeMenu()
  if (!e) return
  try {
    await store.duplicate(props.projectPath, e.path)
    toast.success(t('files.tree.toastDuplicated'))
  } catch (err) { toast.error(String(err)) }
}

// ── Path / open actions ─────────────────────────────────────────────────────
async function copyPath() {
  const e = menu.value?.entry
  closeMenu()
  if (!e) return
  try {
    await navigator.clipboard.writeText(absOf(e.path))
    toast.success(t('files.tree.toastPathCopied'))
  } catch { toast.error(t('files.tree.toastFailedCopyPath')) }
}

async function copyRelPath() {
  const e = menu.value?.entry
  closeMenu()
  if (!e) return
  try {
    await navigator.clipboard.writeText(e.path)
    toast.success(t('files.tree.toastRelativePathCopied'))
  } catch { toast.error(t('files.tree.toastFailedCopyPath')) }
}

async function revealInFinder() {
  const e = menu.value?.entry
  closeMenu()
  if (!e) return
  // Files: open the containing folder; folders: open the folder itself.
  const target = e.is_dir ? absOf(e.path) : absOf(parentDir(e.path))
  try { await invoke('open_in_folder', { path: target }) }
  catch (err) { toast.error(String(err)) }
}

async function openInVscode() {
  const e = menu.value?.entry
  closeMenu()
  if (!e) return
  try {
    if (e.is_dir) await invoke('open_in_vscode', { path: absOf(e.path), file: null })
    else await invoke('open_in_vscode', { path: props.projectPath, file: absOf(e.path) })
  } catch (err) { toast.error(String(err)) }
}

function mentionFile() {
  const e = menu.value?.entry
  closeMenu()
  if (e) emit('mention-file', `${e.path}${e.is_dir ? '/' : ''}`)
}

async function openInChrome() {
  const e = menu.value?.entry
  closeMenu()
  if (!e) return
  try { await invoke('open_in_chrome', { path: absOf(e.path) }) }
  catch (err) { toast.error(String(err)) }
}

// ── Drop onto empty panel area → move to project root ───────────────────────
async function onRootDrop(e: DragEvent) {
  const src = e.dataTransfer?.getData(DND_MIME)
  if (!src || parentDir(src) === '') return
  try { await store.moveInto(props.projectPath, src, '') }
  catch (err) { toast.error(String(err)) }
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
          :class="rootLoading && 'opacity-50 pointer-events-none'"
          @click="store.refresh(projectPath)"
        >
          <RotateCw class="h-3.5 w-3.5" :class="rootLoading && 'animate-spin'" :stroke-width="2" />
        </button>
      </div>
    </div>

    <!-- Body -->
    <div
      class="flex-1 min-h-0 overflow-y-auto py-1"
      @contextmenu.self.prevent="openRootMenu"
      @dragover.prevent
      @drop="onRootDrop"
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
      />
    </div>

    <!-- Right-click context menu (teleported so it floats above everything).
         Kept INSIDE the single root element so `v-show` fallthrough works when
         the host toggles this panel; a second root node would disable it. -->
    <Teleport to="body">
      <div
        v-if="menu"
        ref="menuEl"
        class="fixed z-[100] min-w-[220px] py-1 rounded-md border border-border bg-popover shadow-lg text-xs"
        :style="{ left: `${menu.x}px`, top: `${menu.y}px` }"
        @contextmenu.prevent
      >
        <!-- Create -->
        <button class="w-full flex items-center gap-2 px-3 py-1.5 text-foreground hover:bg-accent cursor-pointer" @click="newFile">
          <FilePlus class="h-3.5 w-3.5" :stroke-width="2" /> {{ t('files.tree.newFile') }}
        </button>
        <button class="w-full flex items-center gap-2 px-3 py-1.5 text-foreground hover:bg-accent cursor-pointer" @click="newFolder">
          <FolderPlus class="h-3.5 w-3.5" :stroke-width="2" /> {{ t('files.tree.newFolder') }}
        </button>

        <!-- Clipboard -->
        <div class="my-1 border-t border-border/60" />
        <template v-if="menuEntry">
          <button class="w-full flex items-center gap-2 px-3 py-1.5 text-foreground hover:bg-accent cursor-pointer" @click="copyToClipboard('copy')">
            <Copy class="h-3.5 w-3.5" :stroke-width="2" /> {{ t('files.tree.copy') }}
          </button>
          <button class="w-full flex items-center gap-2 px-3 py-1.5 text-foreground hover:bg-accent cursor-pointer" @click="copyToClipboard('cut')">
            <Scissors class="h-3.5 w-3.5" :stroke-width="2" /> {{ t('files.tree.cut') }}
          </button>
          <button class="w-full flex items-center gap-2 px-3 py-1.5 text-foreground hover:bg-accent cursor-pointer" @click="duplicate">
            <CopyPlus class="h-3.5 w-3.5" :stroke-width="2" /> {{ t('files.tree.duplicate') }}
          </button>
        </template>
        <button v-if="clipboard" class="w-full flex items-center gap-2 px-3 py-1.5 text-foreground hover:bg-accent cursor-pointer" @click="paste">
          <ClipboardPaste class="h-3.5 w-3.5" :stroke-width="2" /> {{ t('files.tree.paste') }}
        </button>

        <!-- Edit -->
        <template v-if="menuEntry">
          <div class="my-1 border-t border-border/60" />
          <button class="w-full flex items-center gap-2 px-3 py-1.5 text-foreground hover:bg-accent cursor-pointer" @click="renameEntry">
            <Pencil class="h-3.5 w-3.5" :stroke-width="2" /> {{ t('files.tree.rename') }}
          </button>
          <button class="w-full flex items-center gap-2 px-3 py-1.5 text-destructive hover:bg-destructive/10 cursor-pointer" @click="deleteEntry">
            <Trash2 class="h-3.5 w-3.5" :stroke-width="2" /> {{ t('common.delete') }}
          </button>
        </template>

        <!-- Path / open -->
        <template v-if="menuEntry">
          <div class="my-1 border-t border-border/60" />
          <button class="w-full flex items-center gap-2 px-3 py-1.5 text-foreground hover:bg-accent cursor-pointer" @click="copyPath">
            <Copy class="h-3.5 w-3.5" :stroke-width="2" /> {{ t('files.tree.copyPath') }}
          </button>
          <button class="w-full flex items-center gap-2 px-3 py-1.5 text-foreground hover:bg-accent cursor-pointer" @click="copyRelPath">
            <Link class="h-3.5 w-3.5" :stroke-width="2" /> {{ t('files.tree.copyRelativePath') }}
          </button>
          <button class="w-full flex items-center gap-2 px-3 py-1.5 text-foreground hover:bg-accent cursor-pointer" @click="revealInFinder">
            <FolderOpen class="h-3.5 w-3.5" :stroke-width="2" /> {{ t('files.tree.revealInFinder') }}
          </button>
          <button class="w-full flex items-center gap-2 px-3 py-1.5 text-foreground hover:bg-accent cursor-pointer" @click="openInVscode">
            <Code class="h-3.5 w-3.5" :stroke-width="2" /> {{ t('files.tree.openInVscode') }}
          </button>
          <button class="w-full flex items-center gap-2 px-3 py-1.5 text-foreground hover:bg-accent cursor-pointer" @click="openInChrome">
            <Chrome class="h-3.5 w-3.5" :stroke-width="2" /> {{ t('files.tree.openInChrome') }}
          </button>
          <button class="w-full flex items-center gap-2 px-3 py-1.5 text-foreground hover:bg-accent cursor-pointer" @click="mentionFile">
            <AtSign class="h-3.5 w-3.5" :stroke-width="2" /> {{ t('files.tree.mentionInChat') }}
          </button>
        </template>
      </div>
    </Teleport>
  </div>
</template>
