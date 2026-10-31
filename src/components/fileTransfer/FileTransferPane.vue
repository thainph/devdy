<script setup lang="ts">
// One machine's side of the File Transfer window: breadcrumb, home / up /
// refresh, new folder / rename / delete, and a multi-select list. Rows can be
// dragged to the other pane (pointer-driven: Tauri's native drag-drop handler
// swallows HTML5 drags, see lib/fileTreeDnd.ts); folder rows and the list
// carry `data-transfer-dir` so a drop resolves to a folder.
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { ArrowUp, File, Folder, FolderPlus, Home, Pencil, RefreshCw, Trash2 } from 'lucide-vue-next'
import { Button } from '@/components/ui'
import { useConfirm } from '@/composables/useConfirm'
import { usePrompt } from '@/composables/usePrompt'
import { formatSize, joinPath, parentDir, type FsEntry } from '@/lib/ipc/nativeCore'
import { breadcrumbs } from '@/lib/fileTransferCore'
import type { PaneFs } from '@/lib/fileTransfer'

const props = defineProps<{
  fs: PaneFs
  title: string
  /** A drag from the other pane (or the OS) is over this pane's folder `dropDir`. */
  dropDir?: string | null
}>()

const emit = defineEmits<{
  /** Rows dragged past the threshold; the window follows the pointer from here. */
  dragStart: [entries: FsEntry[], x: number, y: number]
}>()

const { t, locale } = useI18n()
const { confirm } = useConfirm()
const { prompt } = usePrompt()

const dir = ref('')
const entries = ref<FsEntry[]>([])
const truncated = ref(false)
const loading = ref(false)
const error = ref<string | null>(null)
const selected = ref<Set<string>>(new Set())
/** Anchor of a shift-click range. */
let anchor: string | null = null

const crumbs = computed(() => breadcrumbs(dir.value))
const selectedEntries = computed(() => entries.value.filter((e) => selected.value.has(e.path)))
const dateFmt = computed(() => new Intl.DateTimeFormat(locale.value, { dateStyle: 'medium', timeStyle: 'short' }))

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

async function navigate(path: string, keepSelection = false) {
  loading.value = true
  error.value = null
  try {
    const listing = await props.fs.list(path)
    dir.value = path
    entries.value = listing.entries
    truncated.value = listing.truncated
    const still = new Set(listing.entries.map((e) => e.path))
    selected.value = keepSelection ? new Set([...selected.value].filter((p) => still.has(p))) : new Set()
  } catch (e) {
    error.value = message(e)
  } finally {
    loading.value = false
  }
}

async function goHome() {
  try {
    await navigate(await props.fs.home())
  } catch (e) {
    error.value = message(e)
  }
}

function goUp() {
  const up = parentDir(dir.value)
  if (up) void navigate(up)
}

function refresh() {
  if (dir.value) void navigate(dir.value, true)
}

function onClick(e: FsEntry, ev: MouseEvent) {
  if (ev.shiftKey && anchor) {
    const paths = entries.value.map((x) => x.path)
    const [a, b] = [paths.indexOf(anchor), paths.indexOf(e.path)].sort((x, y) => x - y)
    if (a >= 0) {
      selected.value = new Set(paths.slice(a, b + 1))
      return
    }
  }
  const next = new Set(ev.metaKey || ev.ctrlKey ? selected.value : [])
  if ((ev.metaKey || ev.ctrlKey) && next.has(e.path)) next.delete(e.path)
  else next.add(e.path)
  selected.value = next
  anchor = e.path
}

function onDoubleClick(e: FsEntry) {
  if (e.is_dir) void navigate(e.path)
}

function selectAll(ev: KeyboardEvent) {
  if ((ev.metaKey || ev.ctrlKey) && ev.key.toLowerCase() === 'a') {
    ev.preventDefault()
    selected.value = new Set(entries.value.map((e) => e.path))
  }
}

// ── Pointer drag (threshold, then the window takes over) ────────────────────

const DRAG_THRESHOLD_PX = 4

function onPointerDown(e: FsEntry, ev: PointerEvent) {
  if (ev.button !== 0) return
  const x0 = ev.clientX
  const y0 = ev.clientY
  const move = (m: PointerEvent) => {
    if (Math.hypot(m.clientX - x0, m.clientY - y0) < DRAG_THRESHOLD_PX) return
    cleanup()
    const dragged = selected.value.has(e.path) ? selectedEntries.value : [e]
    if (!selected.value.has(e.path)) selected.value = new Set([e.path])
    emit('dragStart', dragged, m.clientX, m.clientY)
  }
  const cleanup = () => {
    window.removeEventListener('pointermove', move)
    window.removeEventListener('pointerup', cleanup)
  }
  window.addEventListener('pointermove', move)
  window.addEventListener('pointerup', cleanup)
}

// ── Actions ─────────────────────────────────────────────────────────────────

async function newFolder() {
  if (!dir.value) return
  const name = (await prompt({ title: t('fileTransfer.newFolder'), label: t('fileTransfer.folderName') }))?.trim()
  if (!name) return
  try {
    await props.fs.mkdir(joinPath(dir.value, name))
    await navigate(dir.value)
    selected.value = new Set([joinPath(dir.value, name)])
  } catch (e) {
    error.value = message(e)
  }
}

async function rename() {
  const [entry] = selectedEntries.value
  if (!entry || selectedEntries.value.length !== 1) return
  const name = (
    await prompt({ title: t('fileTransfer.rename'), label: t('fileTransfer.newName'), initialValue: entry.name, selectAll: true })
  )?.trim()
  if (!name || name === entry.name) return
  try {
    const renamed = await props.fs.rename(entry.path, name)
    await navigate(dir.value)
    selected.value = new Set([renamed])
  } catch (e) {
    error.value = message(e)
  }
}

async function remove() {
  const victims = selectedEntries.value
  if (victims.length === 0) return
  const ok = await confirm({
    title: t('fileTransfer.deleteTitle'),
    message:
      victims.length === 1
        ? t('fileTransfer.deleteOne', { name: victims[0].name })
        : t('fileTransfer.deleteMany', { count: victims.length }),
    confirmLabel: t('common.delete'),
    cancelLabel: t('common.cancel'),
  })
  if (!ok) return
  const errors: string[] = []
  for (const v of victims) {
    try {
      await props.fs.remove(v.path)
    } catch (e) {
      errors.push(`${v.name}: ${message(e)}`)
    }
  }
  await navigate(dir.value)
  if (errors.length) error.value = errors.join('\n')
}

onMounted(goHome)

defineExpose({ dir, selectedEntries, refresh })
</script>

<template>
  <section
    class="flex min-h-0 min-w-0 flex-col rounded-lg border bg-card"
    :class="dropDir === dir && dir ? 'border-primary ring-1 ring-primary/40' : 'border-border/70'"
    :data-transfer-side="fs.side"
    tabindex="-1"
    @keydown="selectAll"
  >
    <div class="flex h-9 shrink-0 items-center justify-between gap-2 border-b border-border/60 px-3">
      <span class="truncate text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">{{ title }}</span>
      <div class="flex shrink-0 items-center gap-0.5">
        <Button variant="ghost" size="icon-sm" :title="t('fileTransfer.up')" :disabled="!parentDir(dir)" @click="goUp">
          <ArrowUp class="h-3.5 w-3.5" />
        </Button>
        <Button variant="ghost" size="icon-sm" :title="t('fileTransfer.home')" @click="goHome">
          <Home class="h-3.5 w-3.5" />
        </Button>
        <Button variant="ghost" size="icon-sm" :title="t('common.refresh')" :disabled="!dir" @click="refresh">
          <RefreshCw class="h-3.5 w-3.5" :class="loading && 'animate-spin'" />
        </Button>
        <Button variant="ghost" size="icon-sm" :title="t('fileTransfer.newFolder')" :disabled="!dir" @click="newFolder">
          <FolderPlus class="h-3.5 w-3.5" />
        </Button>
        <Button
          variant="ghost"
          size="icon-sm"
          :title="t('fileTransfer.rename')"
          :disabled="selectedEntries.length !== 1"
          @click="rename"
        >
          <Pencil class="h-3.5 w-3.5" />
        </Button>
        <Button
          variant="destructive-ghost"
          size="icon-sm"
          :title="t('common.delete')"
          :disabled="selectedEntries.length === 0"
          @click="remove"
        >
          <Trash2 class="h-3.5 w-3.5" />
        </Button>
      </div>
    </div>

    <nav class="flex min-h-8 shrink-0 flex-wrap items-center gap-0.5 border-b border-border/60 px-2 py-1 font-mono text-[11px]">
      <template v-for="(c, i) in crumbs" :key="c.path">
        <span v-if="i > 1" class="text-muted-foreground/60">/</span>
        <button
          type="button"
          class="max-w-[12rem] truncate rounded px-1 py-0.5 hover:bg-accent"
          :class="i === crumbs.length - 1 ? 'text-foreground' : 'text-muted-foreground'"
          @click="navigate(c.path)"
        >
          {{ c.label }}
        </button>
      </template>
    </nav>

    <p v-if="error" class="whitespace-pre-line break-all px-3 pt-2 text-xs text-destructive">{{ error }}</p>
    <p v-else-if="truncated" class="px-3 pt-2 text-[11px] text-amber-500">
      {{ t('fileTransfer.truncated', { count: entries.length }) }}
    </p>

    <div class="flex shrink-0 items-center gap-2 px-3.5 pb-1 pt-2 text-[10px] uppercase tracking-wide text-muted-foreground/70">
      <span class="flex-1">{{ t('fileTransfer.colName') }}</span>
      <span class="w-20 text-right">{{ t('fileTransfer.colSize') }}</span>
      <span class="w-36 text-right">{{ t('fileTransfer.colModified') }}</span>
    </div>
    <div class="min-h-0 flex-1 overflow-auto px-1.5 pb-1.5" :data-transfer-dir="dir" @click.self="selected = new Set()">
      <p v-if="!loading && !error && entries.length === 0" class="px-2 py-6 text-center text-xs text-muted-foreground">
        {{ t('fileTransfer.empty') }}
      </p>
      <div
        v-for="e in entries"
        :key="e.path"
        class="flex cursor-pointer select-none items-center gap-2 rounded-md px-2 py-1 text-xs"
        :class="[
          selected.has(e.path) ? 'bg-primary/15 text-foreground' : 'hover:bg-accent',
          e.is_dir && dropDir === e.path && 'ring-1 ring-primary',
        ]"
        :data-transfer-dir="e.is_dir ? e.path : undefined"
        @click="onClick(e, $event)"
        @dblclick="onDoubleClick(e)"
        @pointerdown="onPointerDown(e, $event)"
      >
        <Folder v-if="e.is_dir" class="h-4 w-4 shrink-0 text-primary" :stroke-width="1.75" />
        <File v-else class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="1.75" />
        <span class="flex-1 truncate">{{ e.name }}</span>
        <span class="w-20 shrink-0 text-right tabular-nums text-muted-foreground">{{ e.is_dir ? '' : formatSize(e.size) }}</span>
        <span class="w-36 shrink-0 text-right tabular-nums text-muted-foreground">
          {{ e.modified ? dateFmt.format(e.modified) : '' }}
        </span>
      </div>
    </div>
  </section>
</template>
