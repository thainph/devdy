<script setup lang="ts">
// "Insert Slack thread / web page / note / todo" picker for the chat composer —
// the sibling of the saved-prompt dropdown (RunView, BookMarked icon), built on
// the same DropdownMenu. Picking a web page / note / todo does NOT paste its
// content: it emits a one-line reference — `[Web|Note] <title> (id: …)`, or
// `[Todo] (id: …)` — and the AI resolves the id through the devdy MCP tools when
// it needs more. Picking a Slack thread inserts its translated transcript, built
// by the backend (`get_slack_thread_prompt_text`), optionally with the original
// of each message ("Include original", remembered in localStorage).
//
// The host inserts the emitted text at the caret (same contract as a saved
// prompt: own line, focus, never sends).
//
// A segmented All | Slack | Web | Note | Todo filter (remembered in
// localStorage) narrows the kind; todos show open ones unless "Show done" is on.
// Rows: the run's project first, then the rest; newest first. The search box
// filters title / channel / workspace / thread preview / domain / site / note
// body / todo text; ↑ ↓ move, Enter inserts. Slack rows not yet `ready` carry
// their status mark (spinner / error).
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { Check, Globe, Inbox, ListTodo, Search, Slack, StickyNote } from 'lucide-vue-next'
import { DropdownMenu } from '@/components/ui'
import SlackStatusIcon from '@/components/slack/SlackStatusIcon.vue'
import { useSlackText } from '@/components/slack/useSlackText'
import { useToast } from '@/composables/useToast'
import { firstLine, relativeAge } from '@/lib/itemText'
import {
  CAPTURES_CHANGED,
  captureDomain,
  useCapturesStore,
  type Capture,
  type CaptureKind,
} from '@/stores/captures'
import {
  SLACK_THREADS_CHANGED,
  errorCode,
  useSlackThreadsStore,
  type SlackThreadSummary,
} from '@/stores/slackThreads'
import { useNotesStore, type Note } from '@/stores/notes'
import { useTodosStore, type Todo } from '@/stores/todos'

const props = defineProps<{
  /** The run's project: its items are listed first. */
  projectId?: string | null
  disabled?: boolean
}>()

const emit = defineEmits<{
  /** The reference text to insert into the composer. */
  insert: [text: string]
}>()

type PickKind = CaptureKind | 'slack' | 'note' | 'todo'
type Filter = 'all' | PickKind
const FILTER_STORAGE_KEY = 'devdy.capturePicker.filter'
const SLACK_ORIGINAL_STORAGE_KEY = 'devdy.capturePicker.slackOriginal'
const FILTERS: Filter[] = ['all', 'slack', 'web', 'note', 'todo']
const KIND_ICON = { slack: Slack, web: Globe, note: StickyNote, todo: ListTodo } as const

function loadFilter(): Filter {
  try {
    const v = localStorage.getItem(FILTER_STORAGE_KEY)
    return FILTERS.includes(v as Filter) ? (v as Filter) : 'all'
  } catch {
    return 'all'
  }
}

/** One row of any kind; `reference` is what gets inserted. */
interface PickItem {
  key: string
  kind: PickKind
  title: string
  meta: string
  projectId: string | null
  /** ISO time the "newest first" order uses. */
  sortAt: string
  /** Lower-cased text the search box matches. */
  hay: string
  done: boolean
  /** Inserted as-is (web / note / todo); Slack builds its text on pick. */
  reference: string
  /** Slack only: the thread, for its status mark and the transcript request. */
  thread?: SlackThreadSummary
}

const { t } = useI18n()
const { toast } = useToast()
const { errorText } = useSlackText()
const captures = useCapturesStore()
const slack = useSlackThreadsStore()
const notes = useNotesStore()
const todos = useTodosStore()

const open = ref(false)
const query = ref('')
const filter = ref<Filter>(loadFilter())
/** Todos: open ones only unless this is on. */
const showDone = ref(false)
/** Slack: add the original under each translated message. */
const slackOriginal = ref(loadSlackOriginal())
const active = ref(0)
const searchEl = ref<HTMLInputElement | null>(null)
const rowEls = ref<HTMLButtonElement[]>([])
let unlisten: UnlistenFn[] = []

function loadSlackOriginal(): boolean {
  try {
    return localStorage.getItem(SLACK_ORIGINAL_STORAGE_KEY) === '1'
  } catch {
    return false
  }
}

// ── Rows per kind ───────────────────────────────────────────────────────────

function captureItem(c: Capture): PickItem {
  const title = c.title.trim() || t(`capture.${c.kind}.untitled`)
  const meta = [captureDomain(c) || c.siteName?.trim() || '']
  if (c.attachmentCount > 0) meta.push(`📎${c.attachmentCount}`)
  meta.push(relativeAge(c.updatedAt))
  const hay = `${c.title}\n${captureDomain(c)}\n${c.siteName ?? ''}`
  return {
    key: `capture:${c.id}`,
    kind: c.kind,
    title,
    meta: meta.filter(Boolean).join(' · '),
    projectId: c.projectId,
    sortAt: c.updatedAt,
    hay: hay.toLowerCase(),
    done: false,
    reference: reference(c.kind, c.id, title),
  }
}

function slackItem(th: SlackThreadSummary): PickItem {
  const title = th.title.trim() || t('slackThread.untitled')
  const channel = th.channel ? `#${th.channel}` : th.channelId ? `#${th.channelId}` : ''
  const meta = [channel]
  if (th.fileCount > 0) meta.push(`📎${th.fileCount}`)
  meta.push(relativeAge(th.updatedAt))
  return {
    key: `slack:${th.id}`,
    kind: 'slack',
    title,
    meta: meta.filter(Boolean).join(' · '),
    projectId: th.projectId,
    sortAt: th.updatedAt,
    hay: `${th.title}\n${th.channel ?? ''}\n${th.workspace ?? ''}\n${th.preview}`.toLowerCase(),
    done: false,
    reference: '',
    thread: th,
  }
}

function noteItem(n: Note): PickItem {
  const title = n.title.trim() || firstLine(n.content) || t('item.untitled')
  // The body's first line as the source line, unless it already is the title.
  const preview = n.title.trim() ? firstLine(n.content) : ''
  return {
    key: `note:${n.id}`,
    kind: 'note',
    title,
    meta: [preview, relativeAge(n.updated_at)].filter(Boolean).join(' · '),
    projectId: n.project_id,
    sortAt: n.updated_at,
    hay: `${n.title}\n${n.content}`.toLowerCase(),
    done: false,
    reference: reference('note', n.id, title),
  }
}

function todoItem(x: Todo): PickItem {
  const title = firstLine(x.text) || t('item.untitled')
  return {
    key: `todo:${x.id}`,
    kind: 'todo',
    title,
    meta: [x.done ? t('item.statusDone') : '', relativeAge(x.created_at)].filter(Boolean).join(' · '),
    projectId: x.project_id,
    sortAt: x.created_at,
    hay: x.text.toLowerCase(),
    done: x.done,
    reference: reference('todo', x.id),
  }
}

const REF_TAG: Record<PickKind, string> = { slack: 'Slack', web: 'Web', note: 'Note', todo: 'Todo' }

/** The one-line reference the AI gets: kind, title (none for a todo), id. */
function reference(kind: PickKind, id: string, title?: string): string {
  return `[${REF_TAG[kind]}]${title ? ` ${title}` : ''} (id: ${id})`
}

// ── Filtering / grouping ────────────────────────────────────────────────────

const allItems = computed<PickItem[]>(() => [
  ...slack.threads.map(slackItem),
  ...captures.captures.filter((c) => c.kind === 'web').map(captureItem),
  ...notes.notes.map(noteItem),
  ...todos.todos.map(todoItem),
])

/** The current kind, before the search: what "nothing here yet" is judged on. */
const ofFilter = computed(() =>
  allItems.value.filter((it) => {
    if (filter.value !== 'all' && it.kind !== filter.value) return false
    if (it.kind === 'todo' && it.done && !showDone.value) return false
    return true
  }),
)

const filtered = computed(() => {
  const q = query.value.trim().toLowerCase()
  return q ? ofFilter.value.filter((it) => it.hay.includes(q)) : ofFilter.value
})

const byNewest = (a: PickItem, b: PickItem) => b.sortAt.localeCompare(a.sortAt)

/** "This project" first (only when the run has one), then everything else. */
const groups = computed(() => {
  const pid = props.projectId || null
  const mine = pid ? filtered.value.filter((it) => it.projectId === pid).sort(byNewest) : []
  const rest = filtered.value.filter((it) => !pid || it.projectId !== pid).sort(byNewest)
  const out: { key: string; label: string | null; items: PickItem[] }[] = []
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

/** "Nothing here yet" for the current kind. */
const emptyText = computed(() => {
  switch (filter.value) {
    case 'slack':
      return t('slackThread.sidebar.empty')
    case 'web':
      return t(`capture.${filter.value}.empty`)
    case 'note':
      return t('item.emptyNote')
    case 'todo':
      return t('item.emptyTodo')
    default:
      return t('capture.picker.empty')
  }
})

const showDoneToggle = computed(() => filter.value === 'all' || filter.value === 'todo')
const showOriginalToggle = computed(() => filter.value === 'all' || filter.value === 'slack')

async function pick(it: PickItem) {
  if (!it.thread) {
    emit('insert', it.reference)
    return
  }
  // Slack: the translated transcript (§13.9), built by the backend.
  try {
    const res = await slack.promptText(it.thread.id, slackOriginal.value)
    emit('insert', res.text)
    if (res.truncated) toast.info(t('slackThread.picker.truncated'))
  } catch (e) {
    const { code, detail } = errorCode(e)
    toast.error(t('slackThread.picker.failed', { error: errorText(code, detail) }))
  }
}

function toggleSlackOriginal() {
  slackOriginal.value = !slackOriginal.value
  try {
    localStorage.setItem(SLACK_ORIGINAL_STORAGE_KEY, slackOriginal.value ? '1' : '0')
  } catch {
    /* storage unavailable */
  }
  nextTick(() => searchEl.value?.focus())
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

function toggleShowDone() {
  showDone.value = !showDone.value
  nextTick(() => searchEl.value?.focus())
}

function refresh() {
  void slack.fetchList(null)
  void captures.fetchCaptures()
  void notes.fetchNotes()
  void todos.fetchTodos()
}

function onOpenChange(value: boolean) {
  open.value = value
  if (!value) return
  query.value = ''
  active.value = 0
  refresh()
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

watch([query, filter, showDone], () => {
  active.value = 0
})
watch(active, (i) => {
  rowEls.value[i]?.scrollIntoView({ block: 'nearest' })
})

onMounted(async () => {
  try {
    unlisten.push(
      await listen(CAPTURES_CHANGED, () => {
        if (open.value) void captures.fetchCaptures()
      }),
      await listen(SLACK_THREADS_CHANGED, () => {
        if (open.value) void slack.fetchList(null)
      }),
    )
  } catch {
    /* running outside the Tauri shell */
  }
})

onBeforeUnmount(() => {
  unlisten.forEach((off) => off())
  unlisten = []
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

    <div class="w-[22rem]">
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
            <component :is="KIND_ICON[f]" v-if="f !== 'all'" class="h-3 w-3" :stroke-width="2" />
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
        <!-- Todos: open ones by default; done ones on request. -->
        <button
          v-if="showDoneToggle"
          type="button"
          class="flex cursor-pointer items-center gap-1.5 px-0.5 text-[11px] text-muted-foreground hover:text-foreground"
          :aria-pressed="showDone"
          @click="toggleShowDone"
        >
          <span
            class="flex h-3 w-3 items-center justify-center rounded-[3px] border transition-colors"
            :class="showDone ? 'bg-primary border-primary text-primary-foreground' : 'border-muted-foreground/40'"
          >
            <Check v-if="showDone" class="h-2 w-2" :stroke-width="3" />
          </span>
          {{ t('capture.picker.showDone') }}
        </button>
        <!-- Slack: also insert the original under each translated message. -->
        <button
          v-if="showOriginalToggle"
          type="button"
          class="flex cursor-pointer items-center gap-1.5 px-0.5 text-[11px] text-muted-foreground hover:text-foreground"
          :aria-pressed="slackOriginal"
          @click="toggleSlackOriginal"
        >
          <span
            class="flex h-3 w-3 items-center justify-center rounded-[3px] border transition-colors"
            :class="slackOriginal ? 'bg-primary border-primary text-primary-foreground' : 'border-muted-foreground/40'"
          >
            <Check v-if="slackOriginal" class="h-2 w-2" :stroke-width="3" />
          </span>
          {{ t('slackThread.picker.includeOriginal') }}
        </button>
      </div>

      <div class="max-h-72 overflow-y-auto">
        <p
          v-if="ofFilter.length === 0"
          class="px-2.5 py-4 text-center text-[11px] text-muted-foreground"
          @click.stop
        >
          {{ emptyText }}
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
            v-for="it in group.items"
            :key="it.key"
            :ref="(el) => { if (el) rowEls[flat.indexOf(it)] = el as HTMLButtonElement }"
            type="button"
            class="flex w-full cursor-pointer items-start gap-2 rounded px-2.5 py-1.5 text-left transition-colors"
            :class="flat[active] === it ? 'bg-accent' : 'hover:bg-accent'"
            @mouseenter="active = flat.indexOf(it)"
            @click="pick(it)"
          >
            <component
              :is="KIND_ICON[it.kind]"
              class="mt-0.5 h-3.5 w-3.5 shrink-0 text-muted-foreground"
              :stroke-width="1.75"
            />
            <span class="block min-w-0 flex-1">
              <span
                class="block w-full truncate text-[13px] text-popover-foreground"
                :class="it.done && 'text-muted-foreground line-through'"
              >{{ it.title }}</span>
              <span class="block w-full truncate text-[11px] text-muted-foreground">{{ it.meta }}</span>
            </span>
            <SlackStatusIcon
              v-if="it.thread && it.thread.fetchStatus !== 'ready'"
              :thread="it.thread"
              class="mt-0.5"
            />
          </button>
        </template>
      </div>
    </div>
  </DropdownMenu>
</template>
