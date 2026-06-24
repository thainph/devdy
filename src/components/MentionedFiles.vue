<script setup lang="ts">
import { ref, computed, onBeforeUnmount, type Component } from 'vue'
import { Files, FileText, FilePen, FilePlus2, ChevronDown } from 'lucide-vue-next'
import type { StreamEntry } from '@/lib/streamEvents'

const props = defineProps<{
  entries: StreamEntry[]
}>()

const emit = defineEmits<{
  (e: 'open-file', path: string): void
}>()

// Tool input keys that denote a concrete file (not a search directory).
const FILE_PATH_KEYS = ['file_path', 'notebook_path']

function fileTarget(input: unknown): string | null {
  if (!input || typeof input !== 'object') return null
  const obj = input as Record<string, unknown>
  for (const k of FILE_PATH_KEYS) {
    const v = obj[k]
    if (typeof v === 'string' && v.trim()) return v
  }
  return null
}

type Action = 'write' | 'edit' | 'read'
// Higher = more significant; we display the strongest action seen for a file.
const ACTION_RANK: Record<Action, number> = { read: 0, edit: 1, write: 2 }

function actionOf(name: string): Action {
  const n = (name || '').toLowerCase()
  if (n === 'write') return 'write'
  if (n.includes('edit')) return 'edit'
  return 'read'
}

interface MentionedFile {
  path: string
  name: string
  dir: string
  action: Action
  count: number
}

const ACTION_STYLE: Record<Action, { icon: Component; iconClass: string; label: string }> = {
  read: { icon: FileText, iconClass: 'text-sky-500 dark:text-sky-300', label: 'đọc' },
  edit: { icon: FilePen, iconClass: 'text-amber-500 dark:text-amber-300', label: 'sửa' },
  write: { icon: FilePlus2, iconClass: 'text-rose-500 dark:text-rose-300', label: 'tạo' },
}

const files = computed<MentionedFile[]>(() => {
  const map = new Map<string, MentionedFile>()
  for (const e of props.entries) {
    if (e.kind !== 'tool') continue
    const path = fileTarget(e.input)
    if (!path) continue
    const action = actionOf(e.name)
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
      class="flex items-center gap-1.5 rounded-md px-2 py-1 text-[11px] font-medium transition-colors cursor-pointer"
      :class="open ? 'bg-primary/15 text-primary' : 'text-foreground/50 hover:text-foreground/80 hover:bg-card'"
      :title="`${files.length} file được nhắc trong phiên này`"
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
        <span class="text-[11px] font-medium text-foreground/60">Files trong phiên ({{ files.length }})</span>
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
