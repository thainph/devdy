<script setup lang="ts">
// Dashboard History: every session that ran, in every project, latest activity
// first, one continuous list that loads older pages as it scrolls to the end.
// Search is by session name; "Unread" keeps only runs that finished since the
// user last saw them. A click asks the parent to open the run (in its own
// window); the row stops reading as unread right away.
//
// Unread = the persisted read state (`runs.seen_at`, shared with the Remote
// Control Inbox and set by whichever window shows the run), minus what was
// opened from here since the last fetch. A run finishing does not emit
// `runs:list_changed`, so a change in `liveUnread` (this window saw a finish)
// is what re-reads the list then.
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { History, Search, Star, XCircle, CheckCircle2, Square } from 'lucide-vue-next'
import { Input, SegmentedControl } from '@/components/ui'
import { invoke } from '@/lib/tauri'
import {
  CMD_RUN_HISTORY,
  HISTORY_MAX_PAGE,
  HISTORY_PAGE,
  appendHistory,
  historyKind,
  historyTitle,
  historyWhen,
  mergeHistory,
  type HistoryRun,
} from '@/lib/runHistory'
import EngineMark from './EngineMark.vue'

const props = defineProps<{
  now: number
  /** Runs this window saw finish and nobody opened here: a change means "re-read". */
  liveUnread: ReadonlySet<string>
}>()
const emit = defineEmits<{ open: [run: HistoryRun] }>()
const { t } = useI18n()

const items = ref<HistoryRun[]>([])
const query = ref('')
const show = ref<'all' | 'unread'>('all')
const loading = ref(false)
const loaded = ref(false)
const exhausted = ref(false)
const error = ref<string | null>(null)
/** Opened from here: runId → the activity it had then (a later finish is unread again). */
const openedAt = ref<Record<string, string>>({})

const showOptions = computed(() => [
  { value: 'all', label: t('dashboard.history.all') },
  { value: 'unread', label: t('dashboard.history.unread') },
])

function isUnread(r: HistoryRun): boolean {
  if (openedAt.value[r.runId] === r.lastActivityAt) return false
  return r.status !== 'running' && r.unread
}
const unreadCount = computed(() => items.value.filter(isUnread).length)
const rows = computed(() => (show.value === 'unread' ? items.value.filter(isUnread) : items.value))

// ── Fetching ─────────────────────────────────────────────────────────────────
/** Bumped by every reset so a stale response is dropped. */
let generation = 0

async function fetchPage(before: HistoryRun | null, limit: number): Promise<HistoryRun[]> {
  return invoke<HistoryRun[]>(CMD_RUN_HISTORY, {
    query: query.value.trim() || null,
    unreadOnly: show.value === 'unread',
    beforeAt: before?.lastActivityAt ?? null,
    beforeId: before?.runId ?? null,
    limit,
  })
}

/** Start over (search / filter changed). */
async function reset() {
  const gen = ++generation
  loading.value = true
  error.value = null
  exhausted.value = false
  try {
    const page = await fetchPage(null, HISTORY_PAGE)
    if (gen !== generation) return
    items.value = page
    exhausted.value = page.length < HISTORY_PAGE
  } catch (e) {
    if (gen === generation) error.value = String(e)
  } finally {
    if (gen === generation) {
      loading.value = false
      loaded.value = true
    }
  }
}

/** The next, older page (the end of the list came into view). */
async function more() {
  if (loading.value || exhausted.value || !items.value.length) return
  const gen = generation
  loading.value = true
  try {
    const page = await fetchPage(items.value[items.value.length - 1], HISTORY_PAGE)
    if (gen !== generation) return
    items.value = appendHistory(items.value, page)
    exhausted.value = page.length < HISTORY_PAGE
  } catch (e) {
    if (gen === generation) error.value = String(e)
  } finally {
    if (gen === generation) loading.value = false
  }
}

/** Something changed in the runs table: re-read the loaded head, keep the rest. */
async function refresh() {
  if (loading.value) return
  const gen = generation
  const n = Math.min(Math.max(items.value.length, HISTORY_PAGE), HISTORY_MAX_PAGE)
  try {
    const head = await fetchPage(null, n)
    if (gen !== generation) return
    items.value = mergeHistory(items.value, head, items.value.length <= n)
    if (head.length < n) exhausted.value = true
  } catch {
    /* keep what is shown; the next change retries */
  }
}

let searchTimer: ReturnType<typeof setTimeout> | null = null
watch(query, () => {
  if (searchTimer) clearTimeout(searchTimer)
  searchTimer = setTimeout(() => void reset(), 200)
})
watch(show, () => void reset())

let refreshTimer: ReturnType<typeof setTimeout> | null = null
function refreshSoon() {
  if (refreshTimer) clearTimeout(refreshTimer)
  refreshTimer = setTimeout(() => {
    refreshTimer = null
    void refresh()
  }, 400)
}

// A run finishing here changes the list (its finish is stored just before).
watch(() => [...props.liveUnread].sort().join('|'), refreshSoon)

// ── Infinite scroll ──────────────────────────────────────────────────────────
const scroller = ref<HTMLElement | null>(null)
const sentinel = ref<HTMLElement | null>(null)
let observer: IntersectionObserver | null = null

let unlisten: UnlistenFn | null = null
let disposed = false
onMounted(async () => {
  void reset()
  observer = new IntersectionObserver(
    (entries) => {
      if (entries.some((e) => e.isIntersecting)) void more()
    },
    { root: scroller.value, rootMargin: '160px' },
  )
  if (sentinel.value) observer.observe(sentinel.value)
  try {
    const off = await listen('runs:list_changed', refreshSoon)
    if (disposed) off()
    else unlisten = off
  } catch {
    /* outside the Tauri shell */
  }
})
onBeforeUnmount(() => {
  disposed = true
  observer?.disconnect()
  unlisten?.()
  for (const tm of [searchTimer, refreshTimer]) if (tm) clearTimeout(tm)
})

// ── Rows ─────────────────────────────────────────────────────────────────────
function open(r: HistoryRun) {
  openedAt.value = { ...openedAt.value, [r.runId]: r.lastActivityAt }
  emit('open', r)
}
function name(r: HistoryRun): string {
  return historyTitle(r) ?? t('dashboard.untitled')
}
function statusLabel(r: HistoryRun): string {
  return t(`dashboard.history.status.${['running', 'done', 'failed', 'cancelled'].includes(r.status) ? r.status : 'done'}`)
}

// ── Search box (the Dashboard's "/" shortcut lands here) ─────────────────────
const searchBox = ref<HTMLElement | null>(null)
function focusSearch() {
  searchBox.value?.querySelector('input')?.focus()
}
function onSearchKey(e: KeyboardEvent) {
  if (e.key === 'Escape') {
    e.preventDefault()
    ;(e.target as HTMLElement).blur()
  }
}
defineExpose({ focusSearch })
</script>

<template>
  <section class="flex min-h-0 min-w-0 flex-col gap-2" :aria-label="t('dashboard.history.title')">
    <div class="flex min-h-7 flex-wrap items-center gap-2">
      <History class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="2" aria-hidden="true" />
      <h2 class="text-[13px] font-bold">{{ t('dashboard.history.title') }}</h2>
      <span v-if="unreadCount" class="rounded-full bg-primary/15 px-2 text-[11px] font-bold text-primary">{{ t('dashboard.history.unreadCount', { n: unreadCount }) }}</span>
      <span class="ml-auto flex min-w-0 items-center gap-1.5">
        <label ref="searchBox" class="relative min-w-0 flex-[1_1_150px]">
          <span class="sr-only">{{ t('dashboard.history.searchLabel') }}</span>
          <Search class="pointer-events-none absolute top-1/2 left-2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" :stroke-width="1.75" aria-hidden="true" />
          <Input v-model="query" size="xs" class="w-full min-w-[120px] pl-7" :placeholder="t('dashboard.history.search')" @keydown="onSearchKey" />
        </label>
        <SegmentedControl v-model="show" :options="showOptions" :stretch="false" :aria-label="t('dashboard.history.showLabel')" />
      </span>
    </div>

    <div ref="scroller" class="min-h-[240px] overflow-y-auto rounded-lg border border-border/70 bg-card" style="max-height: max(320px, calc(100vh - 330px))">
      <p v-if="error && !items.length" class="px-4 py-5 text-center text-xs text-destructive">{{ t('dashboard.history.loadError', { error }) }}</p>
      <p v-else-if="loaded && !rows.length && !loading" class="px-4 py-5 text-center text-xs text-muted-foreground">
        {{ query.trim() ? t('dashboard.history.noMatch') : show === 'unread' ? t('dashboard.history.noUnread') : t('dashboard.history.empty') }}
      </p>
      <ul class="flex flex-col p-1" role="list">
        <li v-for="r in rows" :key="r.runId">
          <button
            type="button"
            class="flex w-full min-w-0 cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-left hover:bg-accent/60 focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-primary"
            :title="`${name(r)} — ${r.projectName} · ${statusLabel(r)}`"
            @click="open(r)"
          >
            <span class="grid h-4 w-4 shrink-0 place-items-center" aria-hidden="true">
              <span v-if="r.status === 'running'" class="relative flex h-2 w-2">
                <span class="absolute inline-flex h-full w-full animate-ping rounded-full bg-success opacity-60 motion-reduce:animate-none" />
                <span class="relative inline-flex h-2 w-2 rounded-full bg-success" />
              </span>
              <XCircle v-else-if="r.status === 'failed'" class="h-3.5 w-3.5 text-destructive" :stroke-width="2" />
              <Square v-else-if="r.status === 'cancelled'" class="h-3 w-3 text-muted-foreground" :stroke-width="2.25" />
              <CheckCircle2 v-else class="h-3.5 w-3.5 text-success" :stroke-width="2" />
            </span>
            <span class="sr-only">{{ statusLabel(r) }}<template v-if="isUnread(r)">, {{ t('dashboard.history.unreadOne') }}</template></span>
            <span class="min-w-0 flex-1">
              <span class="block truncate text-[13px] leading-tight" :class="isUnread(r) ? 'font-bold text-foreground' : 'text-foreground/90'">{{ name(r) }}</span>
              <span class="flex min-w-0 items-center gap-1 text-[11px] leading-tight text-muted-foreground">
                <EngineMark :engine="r.engine === 'codex' ? 'codex' : 'claude'" size="sm" />
                <span class="truncate">{{ r.projectName }}</span>
              </span>
            </span>
            <span v-if="historyKind(r)" class="shrink-0 rounded border border-border px-1 text-[9px] font-bold text-muted-foreground">{{ historyKind(r) }}</span>
            <span v-if="r.workerCount" class="shrink-0 rounded-full bg-primary/10 px-1.5 text-[10px] font-semibold text-primary">{{ t('dashboard.history.workers', { n: r.workerCount }, r.workerCount) }}</span>
            <Star v-if="r.pinned" class="h-3 w-3 shrink-0 fill-warning text-warning" :stroke-width="2" :aria-label="t('dashboard.history.pinned')" />
            <span v-if="isUnread(r)" class="h-[7px] w-[7px] shrink-0 rounded-full bg-primary" :title="t('dashboard.history.unreadOne')" aria-hidden="true" />
            <span class="w-[68px] shrink-0 text-right font-mono text-[10px] text-muted-foreground tabular-nums">{{ historyWhen(r.lastActivityAt, now) }}</span>
          </button>
        </li>
      </ul>
      <div ref="sentinel" class="h-px" aria-hidden="true" />
      <p v-if="loading && items.length" class="border-t border-border/60 py-2 text-center text-[11px] text-muted-foreground" aria-live="polite">{{ t('dashboard.history.loadingMore') }}</p>
    </div>
  </section>
</template>
