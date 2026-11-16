<script setup lang="ts">
// Work Dashboard — the app's start screen, read at a glance across every
// project. Two views (remembered): "By category" — a summary row, then four
// cards (customers on Slack, todos, PRs waiting for review, projects at risk
// of slipping), each ranked only within itself and ordered by the user (drag
// a card's handle, or "move to the top" from the keyboard), next to a sticky
// column of the running sessions; and "By project" — one traffic-light row
// per project. One project filter applies everywhere. No poller of its own:
// the cards follow the existing stores / events, refresh when the screen
// opens and on ↻; a 60 s clock only keeps "late N / waiting N" honest.
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { openUrl } from '@tauri-apps/plugin-opener'
import { AlertCircle, LayoutDashboard, LayoutGrid, List, RefreshCw } from 'lucide-vue-next'
import { AppSelect, Button, PageHeader, SegmentedControl } from '@/components/ui'
import SlackWidget from '@/components/dashboard/SlackWidget.vue'
import TodosWidget from '@/components/dashboard/TodosWidget.vue'
import PrWidget from '@/components/dashboard/PrWidget.vue'
import ProjectHealthWidget from '@/components/dashboard/ProjectHealthWidget.vue'
import SessionsPanel from '@/components/dashboard/SessionsPanel.vue'
import SummaryRow, { type SummaryTile } from '@/components/dashboard/SummaryRow.vue'
import ProjectList from '@/components/dashboard/ProjectList.vue'
import { useWorkDashboard } from '@/composables/useWorkDashboard'
import { usePrReviewActions } from '@/composables/usePrReviewActions'
import { useToast } from '@/composables/useToast'
import { useConfirm } from '@/composables/useConfirm'
import { useDragSort } from '@/composables/useDragSort'
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
import { projectOptions } from '@/lib/projectOptions'
import { actionableCount } from '@/lib/prPriority'
import type { SlackDashboardRow } from '@/lib/dashboardSlack'
import type { DashboardRun } from '@/lib/workDashboard'
import {
  moveTo, moveToFront, normalizeOrder, prLevel, projectCard, slackCard, todoLevel, type CategoryKey,
} from '@/lib/dashboardLayout'

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
  ...projectOptions(projects.projects),
])

// ── View + card order (remembered) ───────────────────────────────────────────
type DashView = 'category' | 'project'
const VIEW_KEY = 'devdy.dashboard.view'
const ORDER_KEY = 'devdy.dashboard.order'
function readView(): DashView {
  try { return localStorage.getItem(VIEW_KEY) === 'project' ? 'project' : 'category' } catch { return 'category' }
}
function readOrder(): CategoryKey[] {
  try { return normalizeOrder(JSON.parse(localStorage.getItem(ORDER_KEY) ?? 'null')) } catch { return normalizeOrder(null) }
}
const view = ref<DashView>(readView())
watch(view, (v) => {
  try { localStorage.setItem(VIEW_KEY, v) } catch { /* best-effort */ }
})
const viewOptions = computed(() => [
  { value: 'category' as const, label: t('dashboard.view.category'), icon: LayoutGrid },
  { value: 'project' as const, label: t('dashboard.view.project'), icon: List },
])
const order = ref<CategoryKey[]>(readOrder())
watch(order, (o) => {
  try { localStorage.setItem(ORDER_KEY, JSON.stringify(o)) } catch { /* best-effort */ }
})

function projectName(id: string | null): string | null {
  if (!id || projectFilter.value) return null
  return projects.projects.find((p) => p.id === id)?.name ?? null
}

// ── Data ─────────────────────────────────────────────────────────────────────
const now = ref(Date.now())
let clock: ReturnType<typeof setInterval> | null = null
const dash = useWorkDashboard(projectFilter, now)
const refreshing = dash.refreshing

function refresh(force = false) {
  void dash.refresh(force)
}

onMounted(() => {
  void customers.start()
  clock = setInterval(() => { now.value = Date.now() }, 60_000)
})
onBeforeUnmount(() => {
  if (clock) clearInterval(clock)
})

// Set only once a refresh has finished; a source that failed is named next to it.
const lastRefreshLabel = computed(() =>
  dash.lastRefreshAt.value
    ? new Date(dash.lastRefreshAt.value).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
    : '',
)
const failedLabel = computed(() =>
  dash.failedSources.value.map((k) => t(`dashboard.sources.${k}`)).join(', '),
)

// ── Summary row + card order ─────────────────────────────────────────────────
const cardId = (key: CategoryKey) => `dash-${key}`
const flash = ref<CategoryKey | null>(null)
function highlight(key: CategoryKey) {
  flash.value = key
  setTimeout(() => { if (flash.value === key) flash.value = null }, 1200)
}
function jumpTo(key: CategoryKey) {
  const reduce = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches
  document.getElementById(cardId(key))?.scrollIntoView({ behavior: reduce ? 'auto' : 'smooth', block: 'nearest' })
  highlight(key)
}
function bringToFront(key: CategoryKey) {
  order.value = moveToFront(order.value, key)
  highlight(key)
}

// Drag the card headers' handle to reorder (pointer events via `useDragSort`:
// the Tauri webview swallows HTML5 drag & drop). The others slide aside while
// dragging; the order changes once, on drop.
const cardGrid = ref<HTMLElement | null>(null)
const drag = useDragSort({
  container: cardGrid,
  onDrop: (from, to) => {
    const key = order.value[from]
    const target = order.value[to]
    if (!key || !target) return
    order.value = moveTo(order.value, key, target)
    highlight(key)
  },
})
function dropTargetKey(): CategoryKey | null {
  const from = drag.dragFrom.value
  const to = drag.dragTo.value
  return from !== null && to !== null && to !== from ? order.value[to] ?? null : null
}
// An outline (not a ring) on the Card itself: it follows each theme's corner
// radius, and themes that restyle the card's box-shadow do not hide it.
function cardRing(key: CategoryKey) {
  const out = ['h-full']
  const dragged = drag.dragFrom.value !== null && order.value[drag.dragFrom.value] === key
  if (dragged || dropTargetKey() === key || flash.value === key) out.push('outline-2 outline-offset-2 outline-primary/60')
  return out
}

const failed = computed(() => new Set(dash.failedSources.value))
const summary = computed<SummaryTile[]>(() => {
  const tiles: Record<CategoryKey, SummaryTile> = {
    slack: (() => {
      const c = slackCard(dash.slackRows.value)
      const text = c.waiting.length
        ? t('dashboard.summary.slack.waiting', { n: c.waiting.length }, c.waiting.length)
        : c.unassignedNew
          ? t('dashboard.summary.slack.new', { n: c.unassignedNew }, c.unassignedNew)
          : t('dashboard.summary.slack.none')
      return { key: 'slack', label: t('dashboard.slack.title'), text, level: c.level }
    })(),
    todos: (() => {
      const overdue = dash.todos.value.filter((x) => x.group === 'overdue').length
      const today = dash.todos.value.filter((x) => x.group === 'today').length
      const text = overdue || today ? t('dashboard.summary.todos.some', { overdue, today }) : t('dashboard.summary.todos.none')
      return { key: 'todos', label: t('dashboard.todos.title'), text, level: todoLevel(dash.todos.value) }
    })(),
    prs: (() => {
      const n = actionableCount(dash.prs.value)
      const again = dash.prs.value.rows.filter((r) => r.ai === 'updated' && !r.pr.draft).length
      const text = !n
        ? t('dashboard.summary.prs.none')
        : again
          ? t('dashboard.summary.prs.again', { n, again })
          : t('dashboard.summary.prs.some', { n }, n)
      return { key: 'prs', label: t('dashboard.prs.title'), text, level: prLevel(dash.prs.value) }
    })(),
    projects: (() => {
      const c = projectCard(dash.healths.value)
      const text = c.risk.length || c.attention.length
        ? t('dashboard.summary.projects.some', { risk: c.risk.length, attention: c.attention.length })
        : c.ok.length && c.unknown.length
          ? t('dashboard.summary.projects.okUnknown', { n: c.unknown.length })
          : c.ok.length
            ? t('dashboard.summary.projects.ok')
            : c.unknown.length ? t('dashboard.summary.projects.unknown') : t('dashboard.summary.projects.none')
      return { key: 'projects', label: t('dashboard.health.title'), text, level: c.level }
    })(),
  }
  return order.value.map((k) => (failed.value.has(k)
    ? { ...tiles[k], text: t('dashboard.summary.failed'), level: 'none' as const }
    : tiles[k]))
})

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
function linkBoard(projectId: string) {
  router.push({ name: 'project-settings', params: { projectId } }).catch(() => {})
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
      <template #meta>
        <span v-if="lastRefreshLabel" class="hidden text-[11px] text-muted-foreground md:inline">
          {{ t('dashboard.refreshedAt', { time: lastRefreshLabel }) }}
        </span>
        <span
          v-if="failedLabel"
          class="inline-flex items-center gap-1 text-[11px] text-destructive"
          :title="t('dashboard.failedHint')"
        >
          <AlertCircle class="h-3 w-3" :stroke-width="2" />
          {{ t('dashboard.failed', { sources: failedLabel }) }}
        </span>
      </template>
      <template #actions>
        <SegmentedControl v-model="view" :options="viewOptions" :stretch="false" :aria-label="t('dashboard.view.label')" />
        <AppSelect
          :model-value="projectFilter ?? ALL"
          :options="filterOptions"
          size="sm"
          class="w-44"
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
          <RefreshCw class="h-3.5 w-3.5" :class="refreshing && 'animate-spin motion-reduce:animate-none'" :stroke-width="1.75" />
        </Button>
      </template>
    </PageHeader>

    <div class="min-h-0 flex-1 overflow-y-auto px-6 py-4">
      <!-- By category: summary + user-ordered cards | sticky sessions column -->
      <div v-if="view === 'category'" class="grid items-start gap-4 xl:grid-cols-[minmax(0,1fr)_18.75rem]">
        <div class="@container min-w-0 space-y-4">
          <SummaryRow :tiles="summary" @jump="jumpTo" />
          <div ref="cardGrid" class="grid grid-cols-1 gap-4 @3xl:grid-cols-2">
            <div
              v-for="(key, i) in order"
              :id="cardId(key)"
              :key="key"
              :data-drag-index="i"
              class="relative min-w-0 scroll-mt-4"
              :style="drag.itemStyle(i)"
            >
              <SlackWidget
                v-if="key === 'slack'"
                :rows="dash.slackRows.value"
                :error-count="dash.slackErrors.value"
                :tracked-count="dash.slackThreadCount.value"
                :has-customers="customers.customers.length > 0"
                :state="dash.slackState.value"
                :now="now"
                :project-name="projectName"
                movable
                :first="i === 0"
                :class="cardRing(key)"
                @move-to-front="bringToFront(key)"
                @drag-start="drag.start(i, $event)"
                @open="openThread"
                @open-all="openAllThreads"
                @add-todo="addThreadTodo"
                @mark-seen="markThreadSeen"
              />
              <TodosWidget
                v-else-if="key === 'todos'"
                :todos="dash.todos.value"
                :state="dash.todoState.value"
                :now="now"
                :project-name="projectName"
                movable
                :first="i === 0"
                :class="cardRing(key)"
                @move-to-front="bringToFront(key)"
                @drag-start="drag.start(i, $event)"
                @open="openTodo"
                @open-all="openAllTodos"
                @done="markTodoDone"
              />
              <PrWidget
                v-else-if="key === 'prs'"
                :ranked="dash.prs.value"
                :actionable="actionableCount(dash.prs.value)"
                :state="dash.prState.value"
                :now="now"
                :busy="isReviewing"
                movable
                :first="i === 0"
                :class="cardRing(key)"
                @move-to-front="bringToFront(key)"
                @drag-start="drag.start(i, $event)"
                @review="review.handleReview(storeItem($event))"
                @re-review="review.handleReReview(storeItem($event))"
                @open-pr="openPr"
                @add-todo="addPrTodo"
                @map-repo="router.push('/projects')"
                @open-all="openPrInboxWindow()"
              />
              <ProjectHealthWidget
                v-else
                :healths="dash.healths.value"
                :state="dash.healthState.value"
                :now="now"
                movable
                :first="i === 0"
                :class="cardRing(key)"
                @move-to-front="bringToFront(key)"
                @drag-start="drag.start(i, $event)"
                @open-issue="openIssue"
                @open-radar="openRadar"
              />
            </div>
          </div>
        </div>
        <aside class="min-w-0 xl:sticky xl:top-0 xl:max-h-[calc(100dvh-8rem)] xl:overflow-y-auto" :aria-label="t('dashboard.runs.title')">
          <SessionsPanel
            :runs="dash.runs.value"
            :state="dash.runState.value"
            :now="now"
            :stopping="stopping"
            :project-filter="projectFilter"
            @open="openSession"
            @stop="stopSession"
          />
        </aside>
      </div>

      <!-- By project: one traffic-light row per project -->
      <ProjectList
        v-else
        :rows="dash.byProject.value"
        :state="dash.healthState.value"
        @open-radar="openRadar"
        @open-issue="openIssue"
        @link-board="linkBoard"
      />
    </div>
  </div>
</template>
