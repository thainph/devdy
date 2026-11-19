<script setup lang="ts">
// Background processes manager: every process Devdy tracks (sessions, its own
// helpers, and processes left behind by ended sessions or a previous app run).
// A summary strip (the Dashboard's Resources tile, in full) over one table
// grouped Leftovers / Sessions / App services, with filter tabs (`?filter=`),
// search and sortable columns; graceful stop / force kill per entry or per
// process, and bulk cleanup. Opened from the sidebar, the tray, or the
// Dashboard's Resources tile (`?from=dashboard` adds the way back).
//
// Not in App.vue's KeepAlive list: it polls while shown, so the timer must end
// on unmount.
import { computed, onMounted, onUnmounted, reactive, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute, useRouter } from 'vue-router'
import { ChevronRight, Cpu, RefreshCw, Search, Trash2 } from 'lucide-vue-next'
import { Button, Input, PageHeader, Banner, SegmentedControl, type SegmentedOption } from '@/components/ui'
import ProcessEntryRow from '@/components/processes/ProcessEntryRow.vue'
import ProcessSummary from '@/components/processes/ProcessSummary.vue'
import { PROC_GRID } from '@/components/processes/grid'
import '@/components/dashboard/runtime.css'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { useBackgroundProcessesStore } from '@/stores/backgroundProcesses'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useRunsStore } from '@/stores/runs'
import { useWorkspaceTabsStore } from '@/stores/workspaceTabs'
import { focusSessionWindow } from '@/lib/sessionWindow'
import { invoke } from '@/lib/tauri'
import { CMD_RUNTIME_SNAPSHOT, type RuntimeSnapshot } from '@/lib/runtimeSnapshot'
import {
  groupProcessCounts,
  parseProcFilter,
  procSections,
  strayIds,
  type BgEntry,
  type ProcFilter,
  type ProcSort,
} from '@/lib/backgroundProcesses'

defineOptions({ name: 'BackgroundProcessesView' })

const { t } = useI18n()
const route = useRoute()
const router = useRouter()
const store = useBackgroundProcessesStore()
const live = useLiveRunsStore()
const runsStore = useRunsStore()
const tabsStore = useWorkspaceTabsStore()
const { confirm } = useConfirm()
const { toast } = useToast()

const entries = computed(() => store.snapshot?.entries ?? [])
const strays = computed(() => strayIds(entries.value))
const counts = computed(() => groupProcessCounts(entries.value))
const totalCpu = computed(() => entries.value.reduce((n, e) => n + e.cpu, 0))
const totalRss = computed(() => entries.value.reduce((n, e) => n + e.rss_kb, 0))
const leftoverRss = computed(() => entries.value.filter((e) => e.state !== 'active').reduce((n, e) => n + e.rss_kb, 0))
const cpuMax = computed(() => Math.max(0, ...entries.value.map((e) => e.cpu)))
const rssMax = computed(() => Math.max(0, ...entries.value.map((e) => e.rss_kb)))

// ── Filter (kept in the URL so the Dashboard can deep-link), search, sort ──
const filter = computed<ProcFilter>({
  get: () => parseProcFilter(route.query.filter),
  set: (v) => {
    router.replace({ query: { ...route.query, filter: v === 'all' ? undefined : v } }).catch(() => {})
  },
})
const filterOptions = computed<SegmentedOption<ProcFilter>[]>(() =>
  (['all', 'leftovers', 'sessions', 'services'] as const).map((f) => ({
    value: f,
    label: `${t(`processes.filter.${f}`)} ${counts.value[f]}`,
  })),
)
const query = ref('')
const sort = ref<ProcSort>('rss')
const sections = computed(() => procSections(entries.value, filter.value, query.value, sort.value))
const fromDashboard = computed(() => route.query.from === 'dashboard')

const SECTION_META = computed(() => ({
  leftovers: { title: t('processes.sections.leftovers'), hint: t('processes.sections.leftoversHint'), dot: 'bg-warning' },
  sessions: { title: t('processes.sections.sessions'), hint: '', dot: 'bg-primary' },
  services: { title: t('processes.sections.services'), hint: t('processes.sections.servicesHint'), dot: 'bg-muted-foreground' },
}))

const COLUMNS = computed(() => [
  { label: t('processes.col.name'), sort: null, right: false },
  { label: t('processes.col.kind'), sort: null, right: false },
  { label: t('processes.col.state'), sort: null, right: false },
  { label: t('processes.col.procs'), sort: 'procs' as const, right: true },
  { label: t('processes.col.cpu'), sort: 'cpu' as const, right: true },
  { label: t('processes.col.ram'), sort: 'rss' as const, right: true },
  { label: t('processes.col.age'), sort: 'age' as const, right: true },
  { label: '', sort: null, right: true },
])

// Ages tick with each snapshot rather than on their own timer.
const now = ref(Date.now())
watch(
  () => store.snapshot,
  () => (now.value = Date.now()),
)

// Machine RAM for "of 32 GB": read once, the snapshot command is the only source.
const totalMemKb = ref(0)

const busy = reactive(new Set<string>())
const bulkBusy = ref(false)

let endWatch: (() => void) | null = null
onMounted(() => {
  endWatch = store.watch(3000)
  invoke<RuntimeSnapshot>(CMD_RUNTIME_SNAPSHOT)
    .then((s) => (totalMemKb.value = s?.resources.totalMemKb ?? 0))
    .catch(() => {})
})
onUnmounted(() => {
  endWatch?.()
  endWatch = null
})

function reportStopped(n: number) {
  if (n > 0) toast.success(t('processes.toast.stopped', { n }, n))
  else toast.info(t('processes.toast.nothingStopped'))
}

async function run(ids: string[], force: boolean) {
  ids.forEach((id) => busy.add(id))
  try {
    reportStopped(await store.stopEntries(ids, force))
  } catch (e) {
    toast.error(t('processes.toast.failed', { error: String(e) }))
  } finally {
    ids.forEach((id) => busy.delete(id))
  }
}

async function stopEntry(e: BgEntry) {
  // Stopping an active session also cancels the run — worth one confirmation.
  if (e.state === 'active' && e.kind === 'run') {
    const ok = await confirm({
      title: t('processes.confirm.stopSessionTitle'),
      message: t('processes.confirm.stopSessionMessage', { label: e.label }),
      confirmLabel: t('processes.actions.stop'),
    })
    if (!ok) return
  }
  await run([e.id], false)
}

async function forceEntry(e: BgEntry) {
  const ok = await confirm({
    title: t('processes.confirm.forceTitle'),
    message: t('processes.confirm.forceMessage', { label: e.label }),
    confirmLabel: t('processes.actions.forceKill'),
  })
  if (ok) await run([e.id], true)
}

async function killPid(e: BgEntry, pid: number) {
  busy.add(e.id)
  try {
    await store.killProcess(pid)
  } catch (err) {
    toast.error(t('processes.toast.failed', { error: String(err) }))
  } finally {
    busy.delete(e.id)
  }
}

async function stopLeftovers() {
  const n = counts.value.leftovers
  const ok = await confirm({
    title: t('processes.confirm.stopLeftoversTitle'),
    message: t('processes.confirm.stopLeftoversMessage', { n }, n),
    confirmLabel: t('processes.stopAllLeftovers'),
  })
  if (!ok) return
  bulkBusy.value = true
  try {
    await run(strays.value, false)
  } finally {
    bulkBusy.value = false
  }
}

async function stopEverything() {
  const ok = await confirm({
    title: t('processes.confirm.stopAllTitle'),
    message: t('processes.confirm.stopAllMessage', { n: counts.value.all }),
    confirmLabel: t('processes.stopAll'),
  })
  if (!ok) return
  bulkBusy.value = true
  try {
    await run(entries.value.map((e) => e.id), false)
  } finally {
    bulkBusy.value = false
  }
}

// Same path as the tray: the run's own pop-out if it has one,
// otherwise its workspace tab in this window.
async function openSession(e: BgEntry) {
  const runId = e.run_id
  if (!runId) return
  if (await focusSessionWindow(runId)) return
  const projectId = e.project_id ?? runsStore.runMeta.get(runId)?.project_id ?? null
  if (!projectId) return
  live.markSeen(runId)
  tabsStore.open(projectId, runId)
  router.push({ name: 'project-run-detail', params: { projectId, runId } }).catch(() => {})
}
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- Page header -->
    <PageHeader :title="t('processes.title')" :count="counts.all > 0 ? counts.all : null">
      <template v-if="fromDashboard" #leading>
        <button
          type="button"
          class="flex cursor-pointer items-center gap-1 rounded px-1 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          @click="router.push({ name: 'dashboard' })"
        >
          {{ t('processes.backToDashboard') }}
        </button>
        <ChevronRight class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="2" aria-hidden="true" />
      </template>
      <template #actions>
        <Button variant="outline" size="icon" :title="t('processes.refresh')" :aria-label="t('processes.refresh')" @click="store.refresh()">
          <RefreshCw class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
        <Button
          v-if="strays.length > 0"
          variant="destructive"
          :disabled="bulkBusy"
          @click="stopLeftovers"
        >
          <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('processes.stopAllLeftovers') }}
        </Button>
        <Button v-if="entries.length > 0" variant="outline" :disabled="bulkBusy" @click="stopEverything">
          {{ t('processes.stopAll') }}
        </Button>
      </template>
    </PageHeader>

    <!-- Content -->
    <div class="flex-1 overflow-auto p-6">
      <!-- Loading skeleton (first snapshot only) -->
      <div v-if="store.loading && !store.snapshot" class="space-y-3">
        <div class="grid grid-cols-2 gap-3 xl:grid-cols-4">
          <div v-for="i in 4" :key="i" class="h-32 rounded-lg border border-border bg-card animate-pulse" />
        </div>
        <div class="h-64 rounded-lg border border-border bg-card animate-pulse" />
      </div>

      <!-- Error with nothing to show -->
      <Banner
        v-else-if="store.error && !store.snapshot"
        tone="destructive"
      >
        {{ store.error }}
      </Banner>

      <!-- Empty state -->
      <div v-else-if="entries.length === 0" class="flex flex-col items-center justify-center h-full min-h-80 text-center">
        <div class="flex h-12 w-12 items-center justify-center rounded-xl bg-muted mb-4">
          <Cpu class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
        </div>
        <p class="text-sm font-medium">{{ t('processes.empty.title') }}</p>
        <p class="text-xs text-muted-foreground mt-1 max-w-80">{{ t('processes.empty.hint') }}</p>
      </div>

      <div v-else class="space-y-4">
        <ProcessSummary
          :cpu="totalCpu"
          :cpu-history="store.cpuHistory"
          :rss-kb="totalRss"
          :rss-history="store.rssHistory"
          :total-mem-kb="totalMemKb"
          :counts="counts"
          :group-count="entries.length"
          :leftover-rss-kb="leftoverRss"
          @review="filter = 'leftovers'"
        />

        <!-- Toolbar -->
        <div class="flex flex-wrap items-center gap-2">
          <SegmentedControl v-model="filter" :options="filterOptions" :stretch="false" />
          <div class="relative w-64 max-w-full">
            <Search class="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" :stroke-width="1.75" />
            <Input v-model="query" type="search" size="sm" class="pl-7" :placeholder="t('processes.search')" :aria-label="t('processes.search')" />
          </div>
          <span class="ml-auto text-[11px] text-muted-foreground">{{ t('processes.autoRefresh') }}</span>
        </div>

        <!-- Table -->
        <div class="overflow-x-auto rounded-lg border border-border/70 bg-card">
          <div class="min-w-[920px]">
            <div class="grid items-center gap-3 border-b border-border/60 px-3 py-2 text-[11px] text-muted-foreground" :class="PROC_GRID">
              <template v-for="(c, i) in COLUMNS" :key="i">
                <button
                  v-if="c.sort"
                  type="button"
                  class="cursor-pointer text-right hover:text-foreground"
                  :class="sort === c.sort && 'font-semibold text-foreground'"
                  :title="t('processes.col.sortBy', { col: c.label })"
                  :aria-pressed="sort === c.sort"
                  @click="sort = c.sort"
                >
                  {{ c.label }}
                </button>
                <span v-else :class="[c.right && 'text-right', i === 0 && 'pl-5.5']">{{ c.label }}</span>
              </template>
            </div>

            <template v-for="s in sections" :key="s.key">
              <div class="flex items-center gap-2 border-b border-border/50 bg-muted/40 px-3 py-1.5 text-[11px]">
                <span class="h-2 w-2 shrink-0 rounded-full" :class="SECTION_META[s.key].dot" />
                <span class="font-semibold">{{ SECTION_META[s.key].title }}</span>
                <span class="text-muted-foreground">({{ s.items.length }})</span>
                <span v-if="SECTION_META[s.key].hint" class="ml-1 truncate text-muted-foreground">{{ SECTION_META[s.key].hint }}</span>
              </div>
              <div class="divide-y divide-border/50 border-b border-border/50 last:border-b-0">
                <ProcessEntryRow
                  v-for="e in s.items"
                  :key="e.id"
                  :entry="e"
                  :now="now"
                  :cpu-max="cpuMax"
                  :rss-max="rssMax"
                  :busy="busy.has(e.id) || bulkBusy"
                  @stop="stopEntry(e)"
                  @force="forceEntry(e)"
                  @open="openSession(e)"
                  @kill-pid="(pid) => killPid(e, pid)"
                />
              </div>
            </template>

            <p v-if="sections.length === 0" class="px-3 py-8 text-center text-xs text-muted-foreground">
              {{ query.trim() ? t('processes.noMatch', { q: query.trim() }) : t('processes.noneInFilter') }}
            </p>
          </div>
          <p class="border-t border-border/60 px-3 py-2 text-[11px] text-muted-foreground">{{ t('processes.signalsHint') }}</p>
        </div>
      </div>
    </div>
  </div>
</template>
