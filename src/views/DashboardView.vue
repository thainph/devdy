<script lang="ts">
// Hand choices (which sessions show their workers) last for the app session:
// kept at module level so leaving the screen and coming back keeps them.
const sessionOpen: Record<string, boolean> = {}
</script>

<script setup lang="ts">
// Runtime Dashboard — the app's start screen. One short row of four tiles on
// top (sessions, 5-hour limits, resources, today), then the space goes to the
// sessions running now (a tree of rows: workers under the session that
// spawned them, the most urgent first) and History (every session that ran,
// latest first, with the unread ones marked). Clicking a session opens it in
// its own window, where it is answered / messaged / stopped.
// Keyboard: / search History · j k select · n next waiting · Enter open · Esc leave the search.
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Activity } from 'lucide-vue-next'
import { Banner, Button, PageHeader } from '@/components/ui'
import '@/components/dashboard/runtime.css'
import StatSessions from '@/components/dashboard/StatSessions.vue'
import StatQuota from '@/components/dashboard/StatQuota.vue'
import StatResources from '@/components/dashboard/StatResources.vue'
import StatToday from '@/components/dashboard/StatToday.vue'
import SessionTree from '@/components/dashboard/SessionTree.vue'
import HistoryList from '@/components/dashboard/HistoryList.vue'
import { useRuntimeDashboard } from '@/composables/useRuntimeDashboard'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useBackgroundProcessesStore } from '@/stores/backgroundProcesses'
import { strayIds } from '@/lib/backgroundProcesses'
import { openSessionWindow } from '@/lib/sessionWindow'
import type { HistoryRun } from '@/lib/runHistory'
import {
  buildTree,
  displayOrder,
  filterCounts,
  layoutTree,
  parentOf,
  stepOrder,
  stepQueue,
  waitQueue,
  type QuickFilter,
  type RtSession,
} from '@/lib/runtimeDashboard'

defineOptions({ name: 'DashboardView' })

const { t, locale } = useI18n()
const { confirm, state: confirmState } = useConfirm()
const { toast } = useToast()
const live = useLiveRunsStore()
const bgProcs = useBackgroundProcessesStore()
const dash = useRuntimeDashboard()

// ── Filter / tree ────────────────────────────────────────────────────────────
const filter = ref<QuickFilter | null>(null)
const openMap = ref<Record<string, boolean>>({ ...sessionOpen })
watch(openMap, (m) => Object.assign(sessionOpen, m), { deep: true })

const total = computed(() => dash.sessions.value.length)
const nodes = computed(() => buildTree(dash.sessions.value))
const rows = computed(() => layoutTree(nodes.value, { query: '', filter: filter.value, open: openMap.value }))
const order = computed(() => displayOrder(rows.value))
const queue = computed(() => waitQueue(dash.sessions.value))
const counts = computed(() => filterCounts(dash.sessions.value))
const parents = computed(() => nodes.value.filter((n) => n.children.length).map((n) => n.session.id))

function toggleWorkers(id: string) {
  const r = rows.value.find((x) => x.session.id === id)
  openMap.value = { ...openMap.value, [id]: !(r?.open ?? false) }
}
function setAllOpen(on: boolean) {
  openMap.value = { ...openMap.value, ...Object.fromEntries(parents.value.map((id) => [id, on])) }
}
function toggleFilter(f: QuickFilter) {
  filter.value = filter.value === f ? null : f
}
// A legend row hides at 0: drop its filter then, or nothing could clear it.
watch(counts, (c) => {
  if (filter.value && !c[filter.value]) filter.value = null
})

/** Finished runs this window flagged that the user has not opened yet. */
const liveUnread = computed(() => {
  const ids = new Set<string>()
  live.sessions.forEach((s, id) => {
    if (s.notifyDone && s.status !== 'running') ids.add(id)
  })
  return ids
})

// ── Selection (keyboard) ────────────────────────────────────────────────────
// j / k / n move a highlight; Enter or a click opens the session.
const selectedId = ref<string | null>(null)
const selected = computed<RtSession | null>(() => dash.sessions.value.find((s) => s.id === selectedId.value) ?? null)

/** Highlight a session: a worker's parent shows its workers, and it scrolls into view. */
function select(id: string | null) {
  if (!id) return
  selectedId.value = id
  const parent = parentOf(nodes.value, id)
  if (parent && !rows.value.find((r) => r.session.id === parent)?.open) openMap.value = { ...openMap.value, [parent]: true }
  void nextTick(() => {
    const el = document.querySelector<HTMLElement>(`[data-session-id="${CSS.escape(id)}"]`)
    const reduce = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches
    el?.scrollIntoView({ block: 'nearest', behavior: reduce ? 'auto' : 'smooth' })
  })
}

// ── Opening things ───────────────────────────────────────────────────────────
/** A session always opens in its own window (or brings that window forward). */
function openRun(projectId: string, runId: string) {
  live.markSeen(runId)
  void openSessionWindow(projectId, runId).catch((e) => toast.error(String(e)))
}
/** A session outside Devdy opens the run its transcript was mirrored into, when there is one. */
function openSession(s: RtSession) {
  if (s.runId && s.projectId) return openRun(s.projectId, s.runId)
  const e = s.role === 'external' ? dash.externalById.value.get(s.id) : null
  if (e?.runId && e.projectId) openRun(e.projectId, e.runId)
  else toast.info(t('dashboard.transcriptUnavailable'))
}
function openById(id: string) {
  selectedId.value = id
  const s = dash.sessions.value.find((x) => x.id === id)
  if (s) openSession(s)
}
function openHistory(r: HistoryRun) {
  openRun(r.projectId, r.runId)
}

// ── Cleanup (the Background processes action) ────────────────────────────────
const cleaning = ref(false)
async function cleanUp() {
  if (cleaning.value) return
  cleaning.value = true
  try {
    await bgProcs.refresh()
    const entries = bgProcs.snapshot?.entries ?? []
    const ids = strayIds(entries)
    const n = entries.filter((e) => e.state !== 'active').reduce((sum, e) => sum + e.processes.length, 0)
    if (!ids.length) {
      toast.info(t('processes.toast.nothingStopped'))
      return
    }
    const ok = await confirm({
      title: t('processes.confirm.stopLeftoversTitle'),
      message: t('processes.confirm.stopLeftoversMessage', { n }, n),
      confirmLabel: t('processes.stopAllLeftovers'),
    })
    if (!ok) return
    const stopped = await bgProcs.stopEntries(ids, false)
    if (stopped > 0) toast.success(t('processes.toast.stopped', { n: stopped }, stopped))
    else toast.info(t('processes.toast.nothingStopped'))
    void dash.refresh()
  } catch (e) {
    toast.error(t('processes.toast.failed', { error: String(e) }))
  } finally {
    cleaning.value = false
  }
}

// ── Keyboard ─────────────────────────────────────────────────────────────────
const history = ref<InstanceType<typeof HistoryList> | null>(null)

function typingIn(el: EventTarget | null): boolean {
  if (!(el instanceof HTMLElement)) return false
  return el.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(el.tagName)
}

function onKey(e: KeyboardEvent) {
  if (e.defaultPrevented || e.metaKey || e.ctrlKey || e.altKey || e.isComposing) return
  if (e.repeat || confirmState.open || typingIn(e.target)) return
  switch (e.key) {
    case '/':
      e.preventDefault()
      history.value?.focusSearch()
      break
    case 'j':
      select(stepOrder(order.value, selectedId.value, 1))
      break
    case 'k':
      select(stepOrder(order.value, selectedId.value, -1))
      break
    case 'n':
      select(stepQueue(queue.value, selectedId.value, 1))
      break
    case 'Enter':
      // A focused button handles Enter itself.
      if (e.target instanceof HTMLElement && ['BUTTON', 'A'].includes(e.target.tagName)) return
      if (selected.value) openSession(selected.value)
      break
  }
}
onMounted(() => window.addEventListener('keydown', onKey))
onBeforeUnmount(() => window.removeEventListener('keydown', onKey))

const today = computed(() =>
  new Date(dash.now.value).toLocaleDateString(locale.value, { weekday: 'long', day: '2-digit', month: '2-digit' }),
)
const SHORTCUTS = [
  { keys: ['/'], label: 'search' },
  { keys: ['j', 'k'], label: 'pick' },
  { keys: ['n'], label: 'next' },
  { keys: ['Enter'], label: 'open' },
] as const
</script>

<template>
  <div class="flex h-full flex-col">
    <PageHeader :title="t('dashboard.title')">
      <template #leading>
        <Activity class="h-4 w-4 shrink-0 text-primary" :stroke-width="2" />
      </template>
      <template #meta>
        <span class="hidden truncate text-[11px] text-muted-foreground md:inline">{{ t('dashboard.subtitle', { date: today }) }}</span>
      </template>
      <template #actions>
        <span class="hidden flex-wrap items-center gap-1.5 text-[10px] text-muted-foreground lg:flex" :aria-label="t('dashboard.shortcuts.label')">
          <template v-for="(k, i) in SHORTCUTS" :key="k.label">
            <span v-if="i" aria-hidden="true">·</span>
            <kbd v-for="key in k.keys" :key="key" class="rounded border border-b-2 border-border bg-card px-1 font-mono text-[10px] text-foreground">{{ key }}</kbd>
            <span>{{ t(`dashboard.shortcuts.${k.label}`) }}</span>
          </template>
        </span>
      </template>
    </PageHeader>

    <div class="min-h-0 flex-1 overflow-y-auto px-6 py-4">
      <div class="@container flex flex-col gap-3.5">
        <Banner v-if="dash.loadError.value" tone="destructive">{{ t('dashboard.loadError', { error: dash.loadError.value }) }}</Banner>
        <p v-else-if="dash.loaded.value && !dash.hasSnapshot.value" class="text-[11px] text-muted-foreground">{{ t('dashboard.fallbackHint') }}</p>
        <p v-else-if="dash.snapshotStale.value" class="text-[11px] text-warning">{{ t('dashboard.staleHint') }}</p>

        <!-- One short row of tiles (two per row, then one, as it narrows). -->
        <div class="grid grid-cols-1 gap-2.5 @min-[720px]:grid-cols-2 @min-[1180px]:grid-cols-[1.5fr_0.9fr_1fr_1.4fr]">
          <StatSessions :total="total" :counts="counts" :filter="filter" :activity="dash.activity.value" @filter="toggleFilter" />
          <StatQuota :quotas="dash.quotas.value" :now="dash.now.value" />
          <StatResources :resources="dash.resources.value" :busy="cleaning" @clean="cleanUp" />
          <StatToday :today="dash.today.value" :now="dash.now.value" />
        </div>

        <div class="grid grid-cols-1 items-start gap-4 @min-[1000px]:grid-cols-[minmax(0,1.55fr)_minmax(320px,1fr)]">
          <section class="flex min-w-0 flex-col gap-2" :aria-label="t('dashboard.running.title')">
            <div class="flex min-h-7 flex-wrap items-center gap-2">
              <h2 class="text-[13px] font-bold">{{ t('dashboard.running.title') }}</h2>
              <span class="rounded-full bg-muted px-2 text-[11px] font-bold text-muted-foreground tabular-nums">{{ total }}</span>
              <span class="ml-auto flex gap-1.5">
                <Button variant="outline" size="xs" :disabled="!parents.length" @click="setAllOpen(true)">{{ t('dashboard.toolbar.expandAll') }}</Button>
                <Button variant="outline" size="xs" :disabled="!parents.length" @click="setAllOpen(false)">{{ t('dashboard.toolbar.collapseAll') }}</Button>
              </span>
            </div>

            <div
              v-if="dash.loaded.value && !total"
              class="flex flex-col items-center gap-2 rounded-lg border-[1.5px] border-dashed border-border px-5 py-8 text-center"
            >
              <p class="text-sm font-bold text-foreground">{{ t('dashboard.emptyTitle') }}</p>
              <p class="max-w-[46ch] text-xs text-muted-foreground">{{ t('dashboard.empty') }}</p>
            </div>
            <p
              v-else-if="filter !== null && !rows.length"
              class="rounded-lg border border-dashed border-border/70 px-4 py-5 text-center text-xs text-muted-foreground"
            >
              {{ t('dashboard.noMatch') }}
            </p>

            <SessionTree
              v-if="rows.length"
              :rows="rows"
              :selected-id="selectedId"
              :now="dash.now.value"
              @toggle="toggleWorkers"
              @select="openById"
            />
          </section>

          <HistoryList ref="history" :now="dash.now.value" :live-unread="liveUnread" @open="openHistory" />
        </div>
      </div>
    </div>
  </div>
</template>
