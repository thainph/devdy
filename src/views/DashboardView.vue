<script lang="ts">
// Hand choices (which sessions show their workers) last for the app session:
// kept at module level so leaving the screen and coming back keeps them.
const sessionOpen: Record<string, boolean> = {}
</script>

<script setup lang="ts">
// Runtime Dashboard — the app's start screen: everything running right now.
// Four stat cards (sessions, 5-hour limits, resources, today), then one list
// of sessions as a tree (workers under the session that spawned them, the most
// urgent first). An overview only: clicking a session opens its own view, where
// it is answered / messaged / stopped.
// Keyboard: / search · j k select · n next waiting · Enter open · Esc leave the search.
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { Activity, Search } from 'lucide-vue-next'
import { Banner, Button, Input, PageHeader, SegmentedControl } from '@/components/ui'
import StatSessions from '@/components/dashboard/StatSessions.vue'
import StatQuota from '@/components/dashboard/StatQuota.vue'
import StatResources from '@/components/dashboard/StatResources.vue'
import StatToday from '@/components/dashboard/StatToday.vue'
import SessionTree from '@/components/dashboard/SessionTree.vue'
import { useRuntimeDashboard } from '@/composables/useRuntimeDashboard'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useWorkspaceTabsStore } from '@/stores/workspaceTabs'
import { useBackgroundProcessesStore } from '@/stores/backgroundProcesses'
import { strayIds } from '@/lib/backgroundProcesses'
import type { RecentRun } from '@/lib/runtimeSnapshot'
import {
  buildTree,
  displayOrder,
  effectiveMode,
  filterCounts,
  layoutTree,
  mapOrder,
  parentOf,
  parseViewMode,
  stepOrder,
  stepQueue,
  waitQueue,
  type QuickFilter,
  type RtSession,
  type ViewMode,
} from '@/lib/runtimeDashboard'

defineOptions({ name: 'DashboardView' })

const { t, locale } = useI18n()
const router = useRouter()
const { confirm, state: confirmState } = useConfirm()
const { toast } = useToast()
const live = useLiveRunsStore()
const tabs = useWorkspaceTabsStore()
const bgProcs = useBackgroundProcessesStore()
const dash = useRuntimeDashboard()

// ── View mode (remembered) ───────────────────────────────────────────────────
const MODE_KEY = 'devdy.dashboard.mode'
function readMode(): ViewMode {
  try { return parseViewMode(localStorage.getItem(MODE_KEY)) } catch { return 'auto' }
}
const mode = ref<ViewMode>(readMode())
watch(mode, (v) => {
  try { localStorage.setItem(MODE_KEY, v) } catch { /* best-effort */ }
})
const modeOptions = computed(() => (['auto', 'cards', 'rows'] as const).map((value) => ({ value, label: t(`dashboard.toolbar.mode.${value}`) })))
const total = computed(() => dash.sessions.value.length)
const shownMode = computed(() => effectiveMode(mode.value, total.value))
const modeHint = computed(() => {
  if (mode.value !== 'auto' || !total.value) return ''
  return shownMode.value === 'rows' ? t('dashboard.toolbar.autoRows', { n: total.value }) : t('dashboard.toolbar.autoCards')
})

// ── Search / filter / tree ──────────────────────────────────────────────────
const query = ref('')
const filter = ref<QuickFilter | null>(null)
const openMap = ref<Record<string, boolean>>({ ...sessionOpen })
watch(openMap, (m) => Object.assign(sessionOpen, m), { deep: true })

const nodes = computed(() => buildTree(dash.sessions.value))
const rows = computed(() => layoutTree(nodes.value, { query: query.value, filter: filter.value, open: openMap.value }))
const order = computed(() => displayOrder(rows.value))
const queue = computed(() => waitQueue(dash.sessions.value))
const mapList = computed(() => mapOrder(nodes.value))
const counts = computed(() => filterCounts(dash.sessions.value))
const filtering = computed(() => !!query.value.trim() || filter.value !== null)
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
// The "waiting for you" chip hides at 0: drop its filter then, or nothing could clear it.
watch(() => counts.value.wait, (n) => {
  if (!n && filter.value === 'wait') filter.value = null
})

// ── Selection (keyboard) ────────────────────────────────────────────────────
// j / k / n move a highlight; Enter or a click opens the session's own view.
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
function openRun(projectId: string, runId: string) {
  live.markSeen(runId)
  tabs.open(projectId, runId)
  router.push({ name: 'project-run-detail', params: { projectId, runId } }).catch(() => {})
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
function openRecent(r: RecentRun) {
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
const searchBox = ref<HTMLElement | null>(null)

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
      searchBox.value?.querySelector('input')?.focus()
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
function onSearchKey(e: KeyboardEvent) {
  if (e.key === 'Escape') {
    e.preventDefault()
    ;(e.target as HTMLElement).blur()
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
      <div class="mx-auto flex max-w-[1480px] flex-col gap-3.5">
        <Banner v-if="dash.loadError.value" tone="destructive">{{ t('dashboard.loadError', { error: dash.loadError.value }) }}</Banner>
        <p v-else-if="dash.loaded.value && !dash.hasSnapshot.value" class="text-[11px] text-muted-foreground">{{ t('dashboard.fallbackHint') }}</p>
        <p v-else-if="dash.snapshotStale.value" class="text-[11px] text-warning">{{ t('dashboard.staleHint') }}</p>

        <div class="grid grid-cols-[repeat(auto-fit,minmax(min(100%,270px),1fr))] gap-3">
          <StatSessions
            :sessions="mapList"
            :counts="counts"
            :filter="filter"
            :selected-id="selectedId"
            @pick="select"
            @filter="toggleFilter"
          />
          <StatQuota :quotas="dash.quotas.value" />
          <StatResources :resources="dash.resources.value" :busy="cleaning" @clean="cleanUp" />
          <StatToday :today="dash.today.value" @open="openRecent" />
        </div>

        <div class="flex min-w-0 flex-col gap-2.5">
          <div class="flex flex-wrap items-center gap-2">
            <label ref="searchBox" class="relative min-w-0 flex-[1_1_220px] sm:max-w-[340px]">
              <span class="sr-only">{{ t('dashboard.toolbar.searchLabel') }}</span>
              <Search class="pointer-events-none absolute top-1/2 left-2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" :stroke-width="1.75" aria-hidden="true" />
              <Input v-model="query" size="xs" class="w-full pl-7" :placeholder="t('dashboard.toolbar.search')" @keydown="onSearchKey" />
            </label>
            <SegmentedControl v-model="mode" :options="modeOptions" :stretch="false" :aria-label="t('dashboard.toolbar.mode.label')" />
            <span v-if="modeHint" class="text-[11px] text-muted-foreground">{{ modeHint }}</span>
            <span class="ml-auto flex gap-1.5">
              <Button variant="outline" size="xs" :disabled="!parents.length" @click="setAllOpen(true)">{{ t('dashboard.toolbar.expandAll') }}</Button>
              <Button variant="outline" size="xs" :disabled="!parents.length" @click="setAllOpen(false)">{{ t('dashboard.toolbar.collapseAll') }}</Button>
            </span>
          </div>

          <p
            v-if="dash.loaded.value && !total"
            class="rounded-lg border border-dashed border-border/70 px-4 py-8 text-center text-xs text-muted-foreground"
          >
            {{ t('dashboard.empty') }}
          </p>
          <p
            v-else-if="filtering && !rows.length"
            class="rounded-lg border border-dashed border-border/70 px-4 py-5 text-center text-xs text-muted-foreground"
          >
            {{ t('dashboard.noMatch') }}
          </p>

          <SessionTree
            v-if="rows.length"
            :rows="rows"
            :mode="shownMode"
            :selected-id="selectedId"
            :now="dash.now.value"
            @toggle="toggleWorkers"
            @select="openById"
          />
        </div>
      </div>
    </div>
  </div>
</template>
