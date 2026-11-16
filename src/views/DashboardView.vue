<script setup lang="ts">
// Work Dashboard — the app's start screen: a glance at the work across every
// project. A "needs you now" strip (each item scrolls to its widget), then
// Slack customers, todos, PRs waiting for review, projects at risk of slipping
// and the running sessions. One project filter applies to every widget. No
// poller of its own: the widgets follow the existing stores / events, refresh
// when the screen opens and on ↻; a 60 s clock only keeps "late N / waiting
// N" honest.
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { openUrl } from '@tauri-apps/plugin-opener'
import { LayoutDashboard, RefreshCw } from 'lucide-vue-next'
import { AppSelect, Button, PageHeader } from '@/components/ui'
import SlackWidget from '@/components/dashboard/SlackWidget.vue'
import TodosWidget from '@/components/dashboard/TodosWidget.vue'
import PrWidget from '@/components/dashboard/PrWidget.vue'
import ProjectHealthWidget from '@/components/dashboard/ProjectHealthWidget.vue'
import RunsStrip from '@/components/dashboard/RunsStrip.vue'
import { useWorkDashboard } from '@/composables/useWorkDashboard'
import { usePrReviewActions } from '@/composables/usePrReviewActions'
import { useToast } from '@/composables/useToast'
import { useConfirm } from '@/composables/useConfirm'
import { useProjectsStore } from '@/stores/projects'
import { useFocusStore } from '@/stores/focus'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useRunsStore } from '@/stores/runs'
import { usePrInboxStore, prKey, type PrInboxItem } from '@/stores/prInbox'
import { useWorkspaceTabsStore } from '@/stores/workspaceTabs'
import { useSlackCustomersStore } from '@/stores/slackCustomers'
import { openSlackWindow } from '@/lib/slackWindow'
import { openItemListWindow } from '@/lib/itemListWindow'
import { openPrInboxWindow } from '@/lib/prInboxWindow'
import { openDeliveryRadarWindow } from '@/lib/deliveryRadarWindow'
import { invoke } from '@/lib/tauri'
import { actionableCount } from '@/lib/prPriority'
import type { SlackDashboardRow } from '@/lib/dashboardSlack'
import type { AttentionKey, DashboardRun } from '@/lib/workDashboard'

defineOptions({ name: 'DashboardView' })

const { t } = useI18n()
const router = useRouter()
const { toast } = useToast()
const { confirm } = useConfirm()
const projects = useProjectsStore()
const focus = useFocusStore()
const live = useLiveRunsStore()
const runsStore = useRunsStore()
const prInbox = usePrInboxStore()
const tabs = useWorkspaceTabsStore()
const customers = useSlackCustomersStore()

// ── Project filter (remembered) ──────────────────────────────────────────────
const FILTER_KEY = 'devdy.dashboard.project'
const ALL = ''
function readFilter(): string | null {
  try { return localStorage.getItem(FILTER_KEY) || null } catch { return null }
}
const projectFilter = ref<string | null>(readFilter())
watch(projectFilter, (v) => {
  try {
    if (v) localStorage.setItem(FILTER_KEY, v)
    else localStorage.removeItem(FILTER_KEY)
  } catch { /* best-effort */ }
})
// A deleted project falls back to "All projects".
watch(() => projects.projects, (list) => {
  if (projectFilter.value && list.length && !list.some((p) => p.id === projectFilter.value)) projectFilter.value = null
})
const filterOptions = computed(() => [
  { value: ALL, label: t('dashboard.allProjects') },
  ...projects.projects.map((p) => ({ value: p.id, label: p.name })),
])

function projectName(id: string | null): string | null {
  if (!id || projectFilter.value) return null
  return projects.projects.find((p) => p.id === id)?.name ?? null
}

// ── Data ─────────────────────────────────────────────────────────────────────
const now = ref(Date.now())
let clock: ReturnType<typeof setInterval> | null = null
const dash = useWorkDashboard(projectFilter, now)
const refreshing = ref(false)
const lastRefresh = ref<number | null>(null)

async function refresh(force = false) {
  refreshing.value = true
  try {
    await dash.refresh(force)
    lastRefresh.value = Date.now()
  } finally {
    refreshing.value = false
  }
}

onMounted(() => {
  void customers.start()
  lastRefresh.value = Date.now()
  clock = setInterval(() => { now.value = Date.now() }, 60_000)
})
onBeforeUnmount(() => {
  if (clock) clearInterval(clock)
})

const lastRefreshLabel = computed(() =>
  lastRefresh.value ? new Date(lastRefresh.value).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : '',
)

// ── "Needs you now" ──────────────────────────────────────────────────────────
const sections: Record<AttentionKey, string> = {
  todos: 'dash-todos', prs: 'dash-prs', runs: 'dash-runs', slack: 'dash-slack', projects: 'dash-projects',
}
const flash = ref<AttentionKey | null>(null)
function jumpTo(key: AttentionKey) {
  document.getElementById(sections[key])?.scrollIntoView({ behavior: 'smooth', block: 'nearest' })
  flash.value = key
  setTimeout(() => { if (flash.value === key) flash.value = null }, 1200)
}
function ringIf(key: AttentionKey) {
  return flash.value === key ? 'rounded-lg ring-2 ring-primary/60 transition-shadow' : 'transition-shadow'
}

// ── Slack actions ────────────────────────────────────────────────────────────
function openThread(id: string) {
  void openSlackWindow({ id })
}
function openAllThreads() {
  void openSlackWindow({ projectId: projectFilter.value })
}
async function markThreadSeen(id: string) {
  try {
    await invoke('mark_slack_thread_seen', { id })
    await dash.loadSlack()
  } catch (e) {
    toast.error(String(e))
  }
}
async function addThreadTodo(row: SlackDashboardRow) {
  const thread = dash.slackThreadById(row.thread.id)
  try {
    await focus.add({
      text: t('dashboard.slack.todoText', { title: row.thread.title || t('slackThread.untitled') }),
      projectId: row.thread.projectId,
      sourceUrl: thread?.threadUrl ?? null,
    })
    toast.success(t('dashboard.todoAdded'))
  } catch (e) {
    toast.error(String(e))
  }
}

// ── Todo actions ─────────────────────────────────────────────────────────────
function openTodo(id: string) {
  void openItemListWindow('todo', { id })
}
function openAllTodos() {
  void openItemListWindow('todo', { projectId: projectFilter.value })
}
async function markTodoDone(id: string) {
  try {
    await focus.update(id, { done: true })
    toast.success(t('dashboard.todos.doneToast'))
  } catch (e) {
    toast.error(String(e))
  }
}

// ── PR actions ───────────────────────────────────────────────────────────────
function openRun(projectId: string, runId: string) {
  live.markSeen(runId)
  tabs.open(projectId, runId)
  router.push({ name: 'project-run-detail', params: { projectId, runId } }).catch(() => {})
}
const review = usePrReviewActions({
  openRun: (item) => { if (item.project_id && item.existing_run_id) openRun(item.project_id, item.existing_run_id) },
  runStatus: (item) => (item.existing_run_id ? live.get(item.existing_run_id)?.status ?? item.existing_run_status : null),
})
/** The store's own item (the dashboard may show a copy with a live status). */
function storeItem(item: PrInboxItem): PrInboxItem {
  return prInbox.items.find((i) => prKey(i) === prKey(item)) ?? item
}
function isReviewing(item: PrInboxItem): boolean {
  return prInbox.reviewingKeys.has(prKey(item))
}
function openPr(item: PrInboxItem) {
  openUrl(item.html_url).catch(() => { /* opener unavailable */ })
}
async function addPrTodo(item: PrInboxItem) {
  try {
    await focus.add({
      text: t('dashboard.prs.todoText', { n: item.number, title: item.title }),
      projectId: item.project_id,
      sourceUrl: item.html_url,
    })
    toast.success(t('dashboard.todoAdded'))
  } catch (e) {
    toast.error(String(e))
  }
}

// ── Radar actions ────────────────────────────────────────────────────────────
function openIssue(url: string) {
  openUrl(url).catch(() => { /* opener unavailable */ })
}
function openRadar(projectId?: string) {
  void openDeliveryRadarWindow(projectId ?? projectFilter.value ?? undefined)
}

// ── Session actions ──────────────────────────────────────────────────────────
const stopping = ref<Set<string>>(new Set())
function openSession(projectId: string, runId: string) {
  openRun(projectId, runId)
}
async function stopSession(row: DashboardRun) {
  const ok = await confirm({
    title: t('dashboard.runs.stopTitle'),
    message: t('dashboard.runs.stopMessage', { name: row.run.title || row.run.project_name }),
    confirmLabel: t('dashboard.runs.stop'),
    variant: 'destructive',
  })
  if (!ok) return
  const id = row.run.run_id
  stopping.value = new Set(stopping.value).add(id)
  try {
    await runsStore.cancelRun(id)
    await dash.loadRuns()
  } catch (e) {
    toast.error(String(e))
  } finally {
    const next = new Set(stopping.value)
    next.delete(id)
    stopping.value = next
  }
}
</script>

<template>
  <div class="flex h-full flex-col">
    <PageHeader :title="t('dashboard.title')">
      <template #leading>
        <LayoutDashboard class="h-4 w-4 shrink-0 text-primary" :stroke-width="2" />
      </template>
      <template #actions>
        <AppSelect
          :model-value="projectFilter ?? ALL"
          :options="filterOptions"
          size="sm"
          class="w-48"
          @update:model-value="projectFilter = $event || null"
        />
        <Button
          variant="outline"
          size="icon"
          :disabled="refreshing"
          :title="t('dashboard.refresh')"
          :aria-label="t('dashboard.refresh')"
          @click="refresh(true)"
        >
          <RefreshCw class="h-3.5 w-3.5" :class="refreshing && 'animate-spin'" :stroke-width="1.75" />
        </Button>
        <span v-if="lastRefreshLabel" class="text-[11px] text-muted-foreground">
          {{ t('dashboard.refreshedAt', { time: lastRefreshLabel }) }}
        </span>
      </template>
    </PageHeader>

    <!-- Needs you now -->
    <div class="flex flex-wrap items-center gap-1.5 border-b border-border/60 px-6 py-2">
      <span class="mr-1 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
        {{ t('dashboard.attention.title') }}
      </span>
      <span v-if="dash.attention.value.length === 0" class="text-[11px] text-success">
        {{ t('dashboard.attention.none') }}
      </span>
      <button
        v-for="a in dash.attention.value"
        :key="a.key"
        type="button"
        class="inline-flex h-6 cursor-pointer items-center gap-1 rounded-full border border-warning/30 bg-warning/10 px-2 text-[11px] text-foreground hover:bg-warning/20"
        @click="jumpTo(a.key)"
      >
        <span class="font-semibold tabular-nums">{{ a.count }}</span>
        {{ t(`dashboard.attention.${a.key}`, { n: a.count }, a.count) }}
      </button>
    </div>

    <div class="min-h-0 flex-1 overflow-y-auto px-6 py-4">
      <div class="grid grid-cols-1 gap-4 xl:grid-cols-2">
        <div :id="sections.slack" :class="ringIf('slack')">
          <SlackWidget
            :rows="dash.slackRows.value"
            :attention-count="dash.slackAttention.value"
            :error-count="dash.slackErrors.value"
            :tracked-count="dash.slackThreadCount.value"
            :has-customers="customers.customers.length > 0"
            :state="dash.slackState.value"
            :now="now"
            :project-name="projectName"
            class="h-full"
            @open="openThread"
            @open-all="openAllThreads"
            @add-todo="addThreadTodo"
            @mark-seen="markThreadSeen"
          />
        </div>
        <div :id="sections.todos" :class="ringIf('todos')">
          <TodosWidget
            :todos="dash.todos.value"
            :state="dash.todoState.value"
            :now="now"
            :project-name="projectName"
            class="h-full"
            @open="openTodo"
            @open-all="openAllTodos"
            @done="markTodoDone"
          />
        </div>
        <div :id="sections.prs" :class="ringIf('prs')">
          <PrWidget
            :ranked="dash.prs.value"
            :actionable="actionableCount(dash.prs.value)"
            :state="dash.prState.value"
            :now="now"
            :busy="isReviewing"
            class="h-full"
            @review="review.handleReview(storeItem($event))"
            @re-review="review.handleReReview(storeItem($event))"
            @open-pr="openPr"
            @add-todo="addPrTodo"
            @map-repo="router.push('/projects')"
            @open-all="openPrInboxWindow()"
          />
        </div>
        <div :id="sections.projects" :class="ringIf('projects')">
          <ProjectHealthWidget
            :healths="dash.healths.value"
            :state="dash.healthState.value"
            :now="now"
            class="h-full"
            @open-issue="openIssue"
            @open-radar="openRadar"
          />
        </div>
        <div :id="sections.runs" class="xl:col-span-2" :class="ringIf('runs')">
          <RunsStrip
            :runs="dash.runs.value"
            :state="dash.runState.value"
            :now="now"
            :stopping="stopping"
            @open="openSession"
            @stop="stopSession"
          />
        </div>
      </div>
    </div>
  </div>
</template>
