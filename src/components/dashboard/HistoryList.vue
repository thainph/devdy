<script setup lang="ts">
// Dashboard History: every session that ran, in every project, latest activity
// first, one continuous list that loads older pages as it scrolls to the end.
// Search is by session name; "Unread" keeps only runs that finished since the
// user last saw them. Laid out like Running: a tray (`.rt-tray`) with the
// header inside and every run a card on it; a read run's card sits back. A click asks the parent to open the run (in its own
// window); the row stops reading as unread right away. A right-click (or the
// Menu key / Shift+F10) opens the shared session menu (`SessionMenu`).
//
// A row: a tile for its kind (session / PR / issue / conductor), its name, then
// project · repo#n (issue / PR) or N workers (conductor) · account, a status
// dot and how long ago it last ran.
//
// Unread = the persisted read state (`runs.seen_at`, shared with the Remote
// Control Inbox and set by whichever window shows the run), minus what was
// changed from here (opened, marked read / unread) since the last fetch. A
// run finishing does not emit `runs:list_changed`, so a change in `liveUnread`
// (this window saw a finish) is what re-reads the list then.
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { History, Search } from 'lucide-vue-next'
import { Input, SegmentedControl } from '@/components/ui'
import { invoke } from '@/lib/tauri'
import {
  CMD_RUN_HISTORY,
  HISTORY_MAX_PAGE,
  HISTORY_PAGE,
  appendHistory,
  historyAgo,
  historyExact,
  historyRepoRef,
  historyTitle,
  historyType,
  mergeHistory,
  type HistoryRun,
} from '@/lib/runHistory'
import SessionKindTile from './SessionKindTile.vue'
import SessionMenu, { menuPoint, type SessionMenuTarget } from './SessionMenu.vue'

const props = defineProps<{
  now: number
  /** Runs this window saw finish and nobody opened here: a change means "re-read". */
  liveUnread: ReadonlySet<string>
}>()
const emit = defineEmits<{ open: [run: HistoryRun, where: 'window' | 'main'] }>()
const { t } = useI18n()

const items = ref<HistoryRun[]>([])
const query = ref('')
/** Opens on "Unread": what finished since the user last looked comes first. */
const show = ref<'all' | 'unread'>('unread')
const loading = ref(false)
const loaded = ref(false)
const exhausted = ref(false)
const error = ref<string | null>(null)
/**
 * Read state set from here (opened, marked read / unread) until the backend's
 * catches up: runId → the state and the activity it had then (a later turn
 * finishing makes the stored state count again).
 */
const readOverride = ref<Record<string, { unread: boolean; at: string }>>({})

const showOptions = computed(() => [
  { value: 'all', label: t('dashboard.history.all') },
  { value: 'unread', label: t('dashboard.history.unread') },
])

function isUnread(r: HistoryRun): boolean {
  if (r.status === 'running') return false
  const o = readOverride.value[r.runId]
  return o && o.at === r.lastActivityAt ? o.unread : r.unread
}
function overrideRead(r: HistoryRun, unread: boolean) {
  readOverride.value = { ...readOverride.value, [r.runId]: { unread, at: r.lastActivityAt } }
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
function open(r: HistoryRun, where: 'window' | 'main' = 'window') {
  overrideRead(r, false)
  emit('open', r, where)
}
function name(r: HistoryRun): string {
  return historyTitle(r) ?? t('dashboard.untitled')
}
const STATUSES = ['running', 'done', 'failed', 'cancelled']
function statusLabel(r: HistoryRun): string {
  return t(`dashboard.history.status.${STATUSES.includes(r.status) ? r.status : 'done'}`)
}
// Idle (turn finished) stays neutral so only a live run reads as green.
const STATUS_DOT: Record<string, string> = {
  done: 'bg-muted-foreground/45',
  failed: 'bg-destructive',
  cancelled: 'bg-warning',
}
function account(r: HistoryRun): string {
  return r.engine === 'codex' ? t('dashboard.engine.codex') : r.accountName ?? t('dashboard.engine.claude')
}
function ago(r: HistoryRun): string {
  const a = historyAgo(r.lastActivityAt, props.now)
  if (!a) return ''
  return a.key === 'date' ? a.date : t(`dashboard.history.ago.${a.key}`, { n: a.n }, a.n)
}

// ── Menu (right-click) ───────────────────────────────────────────────────────
const menu = ref<{ open: boolean; x: number; y: number; target: SessionMenuTarget | null }>({ open: false, x: 0, y: 0, target: null })
function openMenu(r: HistoryRun, e: MouseEvent) {
  menu.value = {
    open: true,
    ...menuPoint(e),
    target: {
      runId: r.runId,
      projectId: r.projectId,
      name: name(r),
      runType: r.runType,
      refNumber: r.refNumber,
      repoPath: r.repoPath,
      repoProvider: r.repoProvider,
      title: historyTitle(r),
      pinned: r.pinned,
      protected: r.protected,
      live: r.status === 'running',
      deletable: r.status !== 'running',
      unread: r.status === 'running' ? null : isUnread(r),
    },
  }
}
function closeMenu() {
  menu.value = { ...menu.value, open: false }
}
function findRun(id: string): HistoryRun | undefined {
  return items.value.find((x) => x.runId === id)
}
function onMenuOpen(target: SessionMenuTarget, where: 'window' | 'main') {
  const r = findRun(target.runId)
  if (r) open(r, where)
}
function onMenuRead(target: SessionMenuTarget, unread: boolean) {
  const r = findRun(target.runId)
  if (r) overrideRead(r, unread)
}
function onDeleted(runId: string) {
  items.value = items.value.filter((x) => x.runId !== runId)
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
  <section class="rt-tray flex min-h-0 min-w-0 flex-col gap-1.5 p-1.5" :aria-label="t('dashboard.history.title')">
    <div class="flex min-h-7 flex-wrap items-center gap-2 pt-1 pr-1 pb-0.5 pl-2">
      <span class="grid h-6 w-6 shrink-0 place-items-center rounded-md bg-primary/15 text-primary" aria-hidden="true">
        <History class="h-3.5 w-3.5" :stroke-width="2" />
      </span>
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

    <div ref="scroller" class="min-h-[240px] overflow-y-auto" style="max-height: max(320px, calc(100vh - 330px))">
      <p v-if="error && !items.length" class="px-4 py-5 text-center text-xs text-destructive">{{ t('dashboard.history.loadError', { error }) }}</p>
      <p v-else-if="loaded && !rows.length && !loading" class="rounded-[9px] border border-dashed border-border/70 px-4 py-5 text-center text-xs text-muted-foreground">
        {{ query.trim() ? t('dashboard.history.noMatch') : show === 'unread' ? t('dashboard.history.noUnread') : t('dashboard.history.empty') }}
      </p>
      <ul class="flex flex-col gap-1.5" role="list">
        <!-- Unread: a raised card; read: a quieter one, flat on the tray. -->
        <li v-for="r in rows" :key="r.runId">
          <button
            type="button"
            class="relative flex w-full min-w-0 cursor-pointer items-center gap-3 rounded-lg border py-[9px] pr-2.5 pl-3 text-left transition-colors hover:border-primary/40 focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-primary motion-reduce:transition-none"
            :class="[
              isUnread(r) ? 'border-border/50 bg-card shadow-sm shadow-black/5' : 'border-transparent bg-card/60 hover:bg-card',
              menu.open && menu.target?.runId === r.runId && 'border-primary/40! bg-card!',
            ]"
            :title="`${name(r)} — ${statusLabel(r)}`"
            @click="open(r)"
            @contextmenu.prevent="openMenu(r, $event)"
          >
            <!-- Unread: a bar on the left edge (plus the bold name and time). -->
            <span v-if="isUnread(r)" class="absolute inset-y-2.5 left-0.5 w-[3px] rounded-full bg-primary" aria-hidden="true" />
            <SessionKindTile :kind="historyType(r)" />
            <!-- The name leads; the second line is one quiet grey. -->
            <span class="flex min-w-0 flex-1 flex-col gap-[3px]">
              <span class="truncate text-[13.5px] leading-tight text-foreground" :class="isUnread(r) ? 'font-semibold' : 'font-medium'">{{ name(r) }}</span>
              <span class="flex min-w-0 items-center gap-[5px] text-[11px] leading-tight text-muted-foreground/85">
                <span class="shrink-0">{{ r.projectName }}</span>
                <template v-if="historyRepoRef(r)">
                  <span class="opacity-45" aria-hidden="true">·</span>
                  <span class="min-w-0 truncate font-mono text-[10.5px]">{{ historyRepoRef(r) }}</span>
                </template>
                <template v-else-if="r.workerCount">
                  <span class="opacity-45" aria-hidden="true">·</span>
                  <span class="shrink-0">{{ t('dashboard.history.workers', { n: r.workerCount }, r.workerCount) }}</span>
                </template>
                <span class="opacity-45" aria-hidden="true">·</span>
                <span class="min-w-0 truncate">{{ account(r) }}</span>
              </span>
            </span>
            <span class="sr-only">{{ statusLabel(r) }}<template v-if="isUnread(r)">, {{ t('dashboard.history.unreadOne') }}</template></span>
            <span class="relative flex h-2 w-2 shrink-0" :title="statusLabel(r)" aria-hidden="true">
              <template v-if="r.status === 'running'">
                <span class="absolute inline-flex h-full w-full animate-ping rounded-full bg-success opacity-60 motion-reduce:animate-none" />
                <span class="relative inline-flex h-2 w-2 rounded-full bg-success" />
              </template>
              <span v-else class="h-2 w-2 rounded-full" :class="STATUS_DOT[r.status] ?? STATUS_DOT.done" />
            </span>
            <span
              class="w-[84px] shrink-0 text-right text-[11px] tabular-nums"
              :class="isUnread(r) ? 'font-semibold text-primary' : 'text-muted-foreground'"
              :title="historyExact(r.lastActivityAt)"
            >{{ ago(r) }}</span>
          </button>
        </li>
      </ul>
      <div ref="sentinel" class="h-px" aria-hidden="true" />
      <p v-if="loading && items.length" class="py-2 text-center text-[11px] text-muted-foreground" aria-live="polite">{{ t('dashboard.history.loadingMore') }}</p>
    </div>

    <SessionMenu
      :open="menu.open"
      :x="menu.x"
      :y="menu.y"
      :target="menu.target"
      @close="closeMenu"
      @open-run="onMenuOpen"
      @read="onMenuRead"
      @changed="refresh"
      @deleted="onDeleted"
    />
  </section>
</template>
