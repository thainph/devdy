<script setup lang="ts">
// "Insert Slack thread / web page" picker for the chat composer — the sibling
// of the saved-prompt dropdown (RunView, BookMarked icon), built on the same
// DropdownMenu. Picking a capture does NOT paste its content: it emits a short
// reference (title, source, id) plus a hint to read it through the devdy MCP
// tools, so the AI pulls the full text + attachments on demand.
//
// The host inserts the emitted text at the caret (same contract as a saved
// prompt: own line, focus, never sends).
//
// A segmented All | Slack | Web filter (remembered in localStorage) narrows the
// kind; rows: the run's project first, then the rest; newest first. The search
// box filters title / channel / workspace / domain / site; ↑ ↓ move, Enter inserts.
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { Globe, Inbox, Search, Slack } from 'lucide-vue-next'
import { DropdownMenu } from '@/components/ui'
import { relativeAge } from '@/lib/itemText'
import {
  CAPTURES_CHANGED,
  captureDomain,
  channelLabel,
  useCapturesStore,
  type Capture,
  type CaptureKind,
} from '@/stores/captures'

const props = defineProps<{
  /** The run's project: its captures are listed first. */
  projectId?: string | null
  disabled?: boolean
}>()

const emit = defineEmits<{
  /** The reference text to insert into the composer. */
  insert: [text: string]
}>()

type Filter = 'all' | CaptureKind
const FILTER_STORAGE_KEY = 'devdy.capturePicker.filter'
const FILTERS: Filter[] = ['all', 'slack', 'web']

function loadFilter(): Filter {
  try {
    const v = localStorage.getItem(FILTER_STORAGE_KEY)
    return FILTERS.includes(v as Filter) ? (v as Filter) : 'all'
  } catch {
    return 'all'
  }
}

const { t } = useI18n()
const store = useCapturesStore()

const open = ref(false)
const query = ref('')
const filter = ref<Filter>(loadFilter())
const active = ref(0)
const searchEl = ref<HTMLInputElement | null>(null)
const rowEls = ref<HTMLButtonElement[]>([])
let unlisten: UnlistenFn | null = null

function titleOf(c: Capture): string {
  return c.title.trim() || t(`capture.${c.kind}.untitled`)
}

const byNewest = (a: Capture, b: Capture) => b.updatedAt.localeCompare(a.updatedAt)

const filtered = computed(() => {
  const q = query.value.trim().toLowerCase()
  return store.captures.filter((c) => {
    if (filter.value !== 'all' && c.kind !== filter.value) return false
    if (!q) return true
    const hay =
      c.kind === 'slack'
        ? `${c.title}\n${c.channel ?? ''}\n${c.workspace ?? ''}`
        : `${c.title}\n${captureDomain(c)}\n${c.siteName ?? ''}`
    return hay.toLowerCase().includes(q)
  })
})

/** "This project" first (only when the run has one), then everything else. */
const groups = computed(() => {
  const pid = props.projectId || null
  const mine = pid ? filtered.value.filter((c) => c.projectId === pid).sort(byNewest) : []
  const rest = filtered.value.filter((c) => !pid || c.projectId !== pid).sort(byNewest)
  const out: { key: string; label: string | null; items: Capture[] }[] = []
  if (mine.length) out.push({ key: 'mine', label: t('capture.picker.thisProject'), items: mine })
  if (rest.length) {
    out.push({
      key: 'rest',
      // A heading only helps when there are two groups to tell apart.
      label: mine.length ? t('capture.picker.otherProjects') : null,
      items: rest,
    })
  }
  return out
})

/** Flat order of the rendered rows, for keyboard navigation. */
const flat = computed(() => groups.value.flatMap((g) => g.items))

function meta(c: Capture): string {
  const parts =
    c.kind === 'slack' ? [channelLabel(c)] : [captureDomain(c) || c.siteName?.trim() || '']
  if (c.attachmentCount > 0) parts.push(`📎${c.attachmentCount}`)
  parts.push(relativeAge(c.updatedAt))
  return parts.filter(Boolean).join(' · ')
}

/** The reference the AI gets: short, English, and pointing at the MCP tools. */
function reference(c: Capture): string {
  if (c.kind === 'web') {
    const domain = captureDomain(c) || c.siteName?.trim() || ''
    return (
      `[Web page] ${titleOf(c)}${domain ? ` (${domain})` : ''} — id: ${c.id}\n` +
      'Read it with the web_pages_read tool (use max_chars/offset or section for long pages).'
    )
  }
  const details = [channelLabel(c)]
  if (c.attachmentCount > 0) details.push(`${c.attachmentCount} files`)
  const shown = details.filter(Boolean)
  const suffix = shown.length ? ` (${shown.join(' · ')})` : ''
  return (
    `[Slack thread] ${titleOf(c)}${suffix} — id: ${c.id}\n` +
    'Read it with the slack_threads_read tool (attachments via slack_threads_read_attachment).'
  )
}

function pick(c: Capture) {
  emit('insert', reference(c))
}

function setFilter(next: Filter) {
  filter.value = next
  try {
    localStorage.setItem(FILTER_STORAGE_KEY, next)
  } catch {
    /* storage unavailable */
  }
  nextTick(() => searchEl.value?.focus())
}

function onOpenChange(value: boolean) {
  open.value = value
  if (!value) return
  query.value = ''
  active.value = 0
  void store.fetchCaptures()
  nextTick(() => searchEl.value?.focus())
}

// Rows are clicked programmatically on Enter so the DropdownMenu's own
// click-to-close runs exactly as for a mouse pick.
function onSearchKeydown(e: KeyboardEvent) {
  if (e.isComposing || e.keyCode === 229) return
  const n = flat.value.length
  if (e.key === 'ArrowDown' && n) {
    e.preventDefault()
    active.value = (active.value + 1) % n
  } else if (e.key === 'ArrowUp' && n) {
    e.preventDefault()
    active.value = (active.value - 1 + n) % n
  } else if (e.key === 'Enter' && n) {
    e.preventDefault()
    rowEls.value[active.value]?.click()
  }
}

watch([query, filter], () => {
  active.value = 0
})
watch(active, (i) => {
  rowEls.value[i]?.scrollIntoView({ block: 'nearest' })
})

onMounted(async () => {
  try {
    unlisten = await listen(CAPTURES_CHANGED, () => {
      if (open.value) void store.fetchCaptures()
    })
  } catch {
    /* running outside the Tauri shell */
  }
})

onBeforeUnmount(() => {
  unlisten?.()
  unlisten = null
})
</script>

<template>
  <DropdownMenu align="left" class="shrink-0" @update:open="onOpenChange">
    <template #trigger>
      <button
        class="inline-flex items-center justify-center h-8 w-8 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer disabled:opacity-50 shrink-0"
        :disabled="disabled"
        :title="t('capture.picker.title')"
        :aria-label="t('capture.picker.title')"
      >
        <Inbox class="h-4 w-4" :stroke-width="2" />
      </button>
    </template>

    <div class="w-80">
      <!-- Kind filter + search. Clicks stop here so they don't close the menu. -->
      <div class="mb-1 space-y-1" @click.stop>
        <div class="flex rounded-md bg-muted p-0.5 text-[11px] font-medium">
          <button
            v-for="f in FILTERS"
            :key="f"
            type="button"
            class="flex flex-1 cursor-pointer items-center justify-center gap-1 rounded px-2 py-1 transition-colors"
            :class="filter === f
              ? 'bg-background text-foreground shadow-sm'
              : 'text-muted-foreground hover:text-foreground'"
            :aria-pressed="filter === f"
            @click="setFilter(f)"
          >
            <Slack v-if="f === 'slack'" class="h-3 w-3" :stroke-width="2" />
            <Globe v-else-if="f === 'web'" class="h-3 w-3" :stroke-width="2" />
            {{ t(`capture.picker.${f}`) }}
          </button>
        </div>
        <div class="relative">
          <Search
            class="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground"
            :stroke-width="1.75"
          />
          <input
            ref="searchEl"
            v-model="query"
            type="text"
            class="h-8 w-full rounded border border-border bg-background pl-7 pr-2 text-xs text-foreground outline-none focus:ring-1 focus:ring-ring"
            :placeholder="t('capture.picker.search')"
            @keydown="onSearchKeydown"
          >
        </div>
      </div>

      <div class="max-h-72 overflow-y-auto">
        <p
          v-if="store.captures.length === 0"
          class="px-2.5 py-4 text-center text-[11px] text-muted-foreground"
          @click.stop
        >
          {{ t('capture.picker.empty') }}
        </p>
        <p
          v-else-if="flat.length === 0"
          class="px-2.5 py-4 text-center text-[11px] text-muted-foreground"
          @click.stop
        >
          {{ t('item.noResults') }}
        </p>
        <template v-for="group in groups" v-else :key="group.key">
          <div
            v-if="group.label"
            class="px-2.5 pb-0.5 pt-1.5 text-[10px] font-medium uppercase tracking-wider text-muted-foreground/70"
            @click.stop
          >
            {{ group.label }}
          </div>
          <button
            v-for="c in group.items"
            :key="c.id"
            :ref="(el) => { if (el) rowEls[flat.indexOf(c)] = el as HTMLButtonElement }"
            type="button"
            class="flex w-full cursor-pointer items-start gap-2 rounded px-2.5 py-1.5 text-left transition-colors"
            :class="flat[active] === c ? 'bg-accent' : 'hover:bg-accent'"
            @mouseenter="active = flat.indexOf(c)"
            @click="pick(c)"
          >
            <component
              :is="c.kind === 'web' ? Globe : Slack"
              class="mt-0.5 h-3.5 w-3.5 shrink-0 text-muted-foreground"
              :stroke-width="1.75"
            />
            <span class="block min-w-0 flex-1">
              <span class="block w-full truncate text-[13px] text-popover-foreground">{{ titleOf(c) }}</span>
              <span class="block w-full truncate text-[11px] text-muted-foreground">{{ meta(c) }}</span>
            </span>
          </button>
        </template>
      </div>
    </div>
  </DropdownMenu>
</template>
