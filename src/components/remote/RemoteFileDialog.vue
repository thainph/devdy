<script setup lang="ts">
// File browser of the controlled machine, shown in a remote-desktop window in
// place of the native open/save dialogs and Finder (see src/lib/ipc/native.ts).
// Mounted on demand in its own small app; resolves through `done`.
import { computed, nextTick, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { ArrowUp, Download, File, Folder, Home, RefreshCw } from 'lucide-vue-next'
import { Button, Input, Modal, AppSelect } from '@/components/ui'
import { downloadRemoteFile, remoteFs } from '@/lib/ipc/native'
import { invoke as realInvoke } from '@tauri-real/core'
import {
  formatSize,
  joinPath,
  matchesFilter,
  parentDir,
  withFilterExtension,
  type DialogFilter,
  type FsEntry,
} from '@/lib/ipc/nativeCore'

const props = withDefaults(defineProps<{
  mode: 'open' | 'save' | 'browse'
  title?: string
  directory?: boolean
  multiple?: boolean
  filters?: DialogFilter[]
  startDir?: string | null
  fileName?: string
}>(), {
  title: undefined,
  directory: false,
  multiple: false,
  filters: () => [],
  startDir: null,
  fileName: '',
})

const emit = defineEmits<{ done: [paths: string[] | null] }>()
const { t, locale } = useI18n()

const dir = ref('')
const pathInput = ref('')
const entries = ref<FsEntry[]>([])
const loading = ref(false)
const error = ref<string | null>(null)
const busy = ref(false)
const selected = ref<Set<string>>(new Set())
const saveName = ref(props.fileName)
const filterIndex = ref('0')
const listEl = ref<HTMLElement | null>(null)

const heading = computed(() => {
  if (props.title) return props.title
  if (props.mode === 'save') return t('remoteFiles.saveTitle')
  if (props.mode === 'browse') return t('remoteFiles.browseTitle')
  return props.directory ? t('remoteFiles.openFolderTitle') : t('remoteFiles.openTitle')
})

const filterOptions = computed(() => [
  ...props.filters.map((f, i) => ({
    value: String(i),
    label: f.extensions.length ? `${f.name} (${f.extensions.map((e) => '.' + e).join(', ')})` : f.name,
  })),
  { value: 'all', label: t('remoteFiles.allFiles') },
])
const activeFilter = computed<DialogFilter | null>(() =>
  filterIndex.value === 'all' ? null : (props.filters[Number(filterIndex.value)] ?? null),
)

/** Files a pick may not return (directory mode / filtered out) are shown dimmed. */
function selectable(e: FsEntry): boolean {
  if (props.mode === 'browse') return true
  if (props.directory) return e.is_dir
  if (props.mode === 'save') return !e.is_dir
  return e.is_dir || matchesFilter(e.name, activeFilter.value)
}
function pickable(e: FsEntry): boolean {
  if (props.directory) return e.is_dir
  return !e.is_dir && (props.mode !== 'open' || matchesFilter(e.name, activeFilter.value))
}

const selectedEntries = computed(() => entries.value.filter((e) => selected.value.has(e.path)))
const existingSaveTarget = computed(() => {
  if (props.mode !== 'save' || !saveName.value.trim()) return false
  const name = withFilterExtension(saveName.value.trim(), activeFilter.value)
  return entries.value.some((e) => e.name === name && !e.is_dir)
})

const canConfirm = computed(() => {
  if (busy.value) return false
  if (props.mode === 'save') return !!saveName.value.trim() && !!dir.value
  if (props.mode === 'browse') return selectedEntries.value.some((e) => !e.is_dir)
  if (props.directory) return !!dir.value
  return selectedEntries.value.some(pickable)
})

const confirmLabel = computed(() => {
  if (props.mode === 'save') return t('common.save')
  if (props.mode === 'browse') return t('remoteFiles.downloadOpen')
  return props.directory && selectedEntries.value.length === 0 ? t('remoteFiles.chooseThisFolder') : t('remoteFiles.open')
})

const dateFmt = computed(() => new Intl.DateTimeFormat(locale.value, { dateStyle: 'medium', timeStyle: 'short' }))

async function navigate(path: string, select?: string) {
  loading.value = true
  error.value = null
  try {
    const list = await remoteFs.list(path)
    dir.value = path
    pathInput.value = path
    entries.value = list
    selected.value = new Set()
    if (select) {
      const hit = list.find((e) => e.name === select)
      if (hit && selectable(hit)) {
        selected.value = new Set([hit.path])
        await nextTick()
        listEl.value?.querySelector('[data-selected="true"]')?.scrollIntoView({ block: 'center' })
      }
    }
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e)
    pathInput.value = dir.value
  } finally {
    loading.value = false
  }
}

async function goHome() {
  try {
    await navigate(await remoteFs.home())
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e)
  }
}

function goUp() {
  const up = parentDir(dir.value)
  if (up) void navigate(up)
}

function onClick(e: FsEntry, ev: MouseEvent) {
  if (!selectable(e)) return
  const multi = props.multiple && (ev.metaKey || ev.ctrlKey)
  const next = new Set(multi ? selected.value : [])
  if (multi && next.has(e.path)) next.delete(e.path)
  else next.add(e.path)
  selected.value = next
  if (props.mode === 'save' && !e.is_dir) saveName.value = e.name
}

function onDoubleClick(e: FsEntry) {
  if (e.is_dir) {
    void navigate(e.path)
    return
  }
  if (!selectable(e)) return
  selected.value = new Set([e.path])
  void confirm()
}

async function confirm() {
  if (!canConfirm.value) return
  if (props.mode === 'save') {
    emit('done', [joinPath(dir.value, withFilterExtension(saveName.value.trim(), activeFilter.value))])
    return
  }
  if (props.mode === 'browse') {
    const file = selectedEntries.value.find((e) => !e.is_dir)
    if (!file) return
    busy.value = true
    error.value = null
    try {
      const local = await downloadRemoteFile(file)
      await realInvoke('plugin:opener|open_path', { path: local })
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
    } finally {
      busy.value = false
    }
    return
  }
  const picked = selectedEntries.value.filter(pickable).map((e) => e.path)
  if (props.directory && picked.length === 0) {
    emit('done', [dir.value])
    return
  }
  emit('done', props.multiple ? picked : picked.slice(0, 1))
}

function cancel() {
  emit('done', null)
}

onMounted(async () => {
  if (props.startDir) {
    await navigate(props.startDir, props.fileName || undefined)
    if (!error.value) return
  }
  await goHome()
})
</script>

<template>
  <Modal :open="true" :title="heading" size="xl" scroll-body :dismiss-on-overlay="false" @close="cancel">
    <div class="flex flex-col h-[60vh]">
      <div class="flex items-center gap-1.5 border-b border-border px-3 py-2">
        <Button variant="ghost" size="icon-sm" :title="t('remoteFiles.up')" :disabled="!parentDir(dir)" @click="goUp">
          <ArrowUp class="h-4 w-4" />
        </Button>
        <Button variant="ghost" size="icon-sm" :title="t('remoteFiles.home')" @click="goHome">
          <Home class="h-4 w-4" />
        </Button>
        <Button variant="ghost" size="icon-sm" :title="t('common.refresh')" :disabled="!dir" @click="navigate(dir)">
          <RefreshCw class="h-4 w-4" :class="loading && 'animate-spin'" />
        </Button>
        <Input
          v-model="pathInput"
          class="flex-1 font-mono text-xs"
          spellcheck="false"
          @keydown.enter="navigate(pathInput.trim())"
        />
      </div>
      <p class="px-3 pt-2 text-[11px] text-muted-foreground">{{ t('remoteFiles.hint') }}</p>
      <p v-if="error" class="px-3 pt-1 text-xs text-destructive break-all">{{ error }}</p>
      <div ref="listEl" class="flex-1 min-h-0 overflow-auto px-1.5 py-1.5">
        <p v-if="!loading && !error && entries.length === 0" class="px-2 py-6 text-center text-xs text-muted-foreground">
          {{ t('remoteFiles.empty') }}
        </p>
        <div
          v-for="e in entries"
          :key="e.path"
          :data-selected="selected.has(e.path)"
          class="flex items-center gap-2 rounded-md px-2 py-1 text-xs select-none"
          :class="[
            selected.has(e.path) ? 'bg-primary/15 text-foreground' : 'hover:bg-accent',
            selectable(e) || e.is_dir ? 'cursor-pointer' : 'opacity-40 cursor-default',
          ]"
          @click="onClick(e, $event)"
          @dblclick="onDoubleClick(e)"
        >
          <Folder v-if="e.is_dir" class="h-4 w-4 shrink-0 text-primary" :stroke-width="1.75" />
          <File v-else class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="1.75" />
          <span class="flex-1 truncate">{{ e.name }}</span>
          <span class="w-20 shrink-0 text-right tabular-nums text-muted-foreground">{{ e.is_dir ? '' : formatSize(e.size) }}</span>
          <span class="w-40 shrink-0 text-right tabular-nums text-muted-foreground">
            {{ e.modified ? dateFmt.format(e.modified) : '' }}
          </span>
        </div>
      </div>
    </div>

    <template #footer>
      <div class="flex flex-1 items-center gap-2 min-w-0">
        <template v-if="mode === 'save'">
          <span class="text-xs text-muted-foreground shrink-0">{{ t('remoteFiles.fileName') }}</span>
          <Input v-model="saveName" class="flex-1 min-w-0" spellcheck="false" @keydown.enter="confirm" />
        </template>
        <AppSelect
          v-if="filters.length && !directory && mode !== 'browse'"
          v-model="filterIndex"
          :options="filterOptions"
          size="sm"
          class="w-56 shrink-0"
        />
        <span v-if="existingSaveTarget" class="text-[11px] text-amber-500 shrink-0">{{ t('remoteFiles.willReplace') }}</span>
      </div>
      <Button variant="outline" @click="cancel">{{ mode === 'browse' ? t('common.close') : t('common.cancel') }}</Button>
      <Button :disabled="!canConfirm" @click="confirm">
        <Download v-if="mode === 'browse'" class="h-3.5 w-3.5" />
        {{ confirmLabel }}
      </Button>
    </template>
  </Modal>
</template>
