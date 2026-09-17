<script setup lang="ts">
import { ref, computed, onBeforeUnmount, type Component } from 'vue'
import { useI18n } from 'vue-i18n'
import { Files, FilePen, FilePlus2, ChevronDown } from 'lucide-vue-next'
import type { StreamEntry } from '@/lib/streamEvents'
// Path extraction lives in `@/lib/toolFiles` so this list and the run log's own
// file links always agree on what a tool call touched — `path` is safe to read
// here because only writing tools reach it (search tools, where `path` means a
// directory, are filtered out by `writeActionOf` first).
import { toolFileTargets } from '@/lib/toolFiles'

const { t } = useI18n()

const props = defineProps<{
  entries: StreamEntry[]
}>()

const emit = defineEmits<{
  (e: 'open-file', path: string): void
}>()

type Action = 'write' | 'edit'
// Higher = more significant; we display the strongest action seen for a file.
const ACTION_RANK: Record<Action, number> = { edit: 0, write: 1 }

// Only tools that actually produce or change a file on disk. Read-only tools
// (Read, Grep, Glob, NotebookRead…) are deliberately excluded: this list answers
// "what did this session write", not "what did it look at".
function writeActionOf(name: string): Action | null {
  const n = (name || '').toLowerCase()
  if (n === 'write') return 'write'
  // Edit, MultiEdit, NotebookEdit, and the `Edit` alias Codex runs are mapped to.
  if (n.includes('edit')) return 'edit'
  return null
}

interface MentionedFile {
  path: string
  name: string
  dir: string
  action: Action
  count: number
}

const ACTION_STYLE = computed<Record<Action, { icon: Component; iconClass: string; label: string }>>(() => ({
  edit: { icon: FilePen, iconClass: 'text-amber-500 dark:text-amber-300', label: t('misc.mentionedFiles.edit') },
  write: { icon: FilePlus2, iconClass: 'text-rose-500 dark:text-rose-300', label: t('misc.mentionedFiles.write') },
}))

const files = computed<MentionedFile[]>(() => {
  const map = new Map<string, MentionedFile>()
  for (const e of props.entries) {
    if (e.kind !== 'tool') continue
    const action = writeActionOf(e.name)
    if (!action) continue
    // A rejected or failed write never reached the disk. A still-running call
    // has no result yet, so it stays listed (optimistic while streaming).
    if (e.result?.is_error) continue
    for (const path of toolFileTargets(e.input)) {
      const existing = map.get(path)
      if (existing) {
        existing.count++
        if (ACTION_RANK[action] > ACTION_RANK[existing.action]) existing.action = action
      } else {
        const slash = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'))
        map.set(path, {
          path,
          name: slash >= 0 ? path.slice(slash + 1) : path,
          dir: slash >= 0 ? path.slice(0, slash) : '',
          action,
          count: 1,
        })
      }
    }
  }
  // Keep first-seen order (insertion order of the Map preserves it).
  return [...map.values()]
})

const open = ref(false)
const rootEl = ref<HTMLElement | null>(null)

function toggle() {
  open.value = !open.value
}

function pick(f: MentionedFile) {
  emit('open-file', f.path)
  open.value = false
}

function onDocClick(e: MouseEvent) {
  if (rootEl.value && !rootEl.value.contains(e.target as Node)) open.value = false
}
function onKey(e: KeyboardEvent) {
  if (e.key === 'Escape') open.value = false
}
// Listeners are cheap and only meaningful while mounted; attach once.
document.addEventListener('click', onDocClick)
document.addEventListener('keydown', onKey)
onBeforeUnmount(() => {
  document.removeEventListener('click', onDocClick)
  document.removeEventListener('keydown', onKey)
})
</script>

<template>
  <div v-if="files.length" ref="rootEl" class="relative">
    <button
      class="flex items-center gap-1.5 rounded-md h-6 px-2 text-[11px] font-medium leading-none transition-colors cursor-pointer"
      :class="open ? 'bg-primary/15 text-primary' : 'text-foreground/50 hover:text-foreground/80 hover:bg-card'"
      :title="t('misc.mentionedFiles.countTitle', { count: files.length })"
      @click.stop="toggle"
    >
      <Files class="h-3.5 w-3.5" :stroke-width="1.75" />
      <span>{{ files.length }}</span>
      <ChevronDown class="h-3 w-3 transition-transform" :class="open && 'rotate-180'" :stroke-width="2" />
    </button>

    <div
      v-if="open"
      class="absolute right-0 top-full z-30 mt-1 w-80 max-w-[80vw] overflow-hidden rounded-md border border-border bg-popover shadow-lg"
    >
      <div class="px-3 py-2 border-b border-border">
        <span class="text-[11px] font-medium text-foreground/60">{{ t('misc.mentionedFiles.filesInSession', { count: files.length }) }}</span>
      </div>
      <ul class="max-h-72 overflow-y-auto py-1">
        <li v-for="f in files" :key="f.path">
          <button
            class="group flex w-full items-center gap-2 px-3 py-1.5 text-left transition-colors hover:bg-card cursor-pointer"
            :title="f.path"
            @click="pick(f)"
          >
            <component
              :is="ACTION_STYLE[f.action].icon"
              class="h-3.5 w-3.5 shrink-0"
              :class="ACTION_STYLE[f.action].iconClass"
              :stroke-width="1.75"
            />
            <span class="min-w-0 flex-1">
              <span class="block truncate text-xs font-medium text-foreground/90">{{ f.name }}</span>
              <span v-if="f.dir" class="block truncate text-[10px] font-mono text-foreground/40">{{ f.dir }}</span>
            </span>
            <span
              v-if="f.count > 1"
              class="shrink-0 rounded-full bg-foreground/10 px-1.5 text-[10px] font-mono text-foreground/50"
            >{{ f.count }}</span>
          </button>
        </li>
      </ul>
    </div>
  </div>
</template>
