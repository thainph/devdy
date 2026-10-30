<script setup lang="ts">
// Focus board (docs/focus-board-spec.md §3.2): every open todo of every project,
// ranked by the backend (BR-04/05) — the UI only groups by bucket and sorts by
// score. Three views: by time ("Do now" top 3 + collapsible buckets), by project
// (one column each), and the week (Mon → Sun columns by due date). Rows open the
// detail drawer. Refreshes on `focus:changed` / `item:changed` and every minute.
// A reminder click (`focus:open-todo`, or the pending id taken on mount when the
// click created this window) opens that todo's drawer and flashes its row.
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { CalendarClock, ChevronDown, ChevronRight, Gauge, RefreshCw, Target, TriangleAlert } from 'lucide-vue-next'
import { invoke } from '@/lib/tauri'
import { AppSelect, Button, Card, Skeleton } from '@/components/ui'
import AddTodoButton from '@/components/focus/AddTodoButton.vue'
import FocusDialogs from '@/components/focus/FocusDialogs.vue'
import FocusTodoDrawer from '@/components/focus/FocusTodoDrawer.vue'
import FocusTodoRow from '@/components/focus/FocusTodoRow.vue'
import { useFocusText } from '@/components/focus/useFocusText'
import { useFocusStore } from '@/stores/focus'
import { useProjectsStore } from '@/stores/projects'
import {
  CAPACITY_LOADING,
  CAPACITY_NO_ACCOUNT,
  DEFAULT_OPEN_BUCKETS,
  FOCUS_OPEN_TODO,
  FILTER_NONE,
  calibrationHint,
  filterItems,
  formatEstimate,
  groupByBucket,
  groupByProject,
  groupByWeek,
  isActiveBucket,
  projectHue,
  topItems,
  ymdOf,
  type FocusBucket,
  type FocusOpenTodo,
} from '@/lib/focus'

type FocusViewMode = 'time' | 'project' | 'week'

const { t, locale } = useI18n()
const store = useFocusStore()
const projects = useProjectsStore()
const { span } = useFocusText()

// ── View + filters (remembered) ──
const VIEW_KEY = 'devdy:focusView'
const OPEN_KEY = 'devdy:focusOpenBuckets'
const FILTER_KEY = 'devdy:focusFilter'

const VIEWS: FocusViewMode[] = ['time', 'project', 'week']
const view = ref<FocusViewMode>(VIEWS.find((v) => v === localStorage.getItem(VIEW_KEY)) ?? 'time')
watch(view, (v) => localStorage.setItem(VIEW_KEY, v))

function readJson<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key)
    return raw ? (JSON.parse(raw) as T) : fallback
  } catch {
    return fallback
  }
}
const openBuckets = ref(new Set<FocusBucket>(readJson<FocusBucket[]>(OPEN_KEY, DEFAULT_OPEN_BUCKETS)))
function toggleBucket(b: FocusBucket) {
  const next = new Set(openBuckets.value)
  if (next.has(b)) next.delete(b)
  else next.add(b)
  openBuckets.value = next
  localStorage.setItem(OPEN_KEY, JSON.stringify([...next]))
}

const filter = ref(readJson(FILTER_KEY, { projectId: '', sourceType: '' }))
watch(filter, (f) => localStorage.setItem(FILTER_KEY, JSON.stringify(f)), { deep: true })

const projectOptions = computed(() => [
  { value: '', label: t('focus.filters.allProjects') },
  ...projects.projects.map((p) => ({ value: p.id, label: p.name })),
  { value: FILTER_NONE, label: t('focus.filters.noProject') },
])
const SOURCES = ['slack', 'github', 'gitlab', 'run', 'url'] as const
const sourceOptions = computed(() => [
  { value: '', label: t('focus.filters.allSources') },
  ...SOURCES.map((s) => ({ value: s, label: t(`focus.sources.${s}`) })),
  { value: FILTER_NONE, label: t('focus.filters.noSource') },
])

// ── Board ──
const now = computed(() => store.now)
const visible = computed(() => filterItems(store.items, filter.value))
const openCount = computed(() => store.items.filter((it) => isActiveBucket(it.bucket)).length)
const top = computed(() => topItems(visible.value, store.board?.top ?? []))
const buckets = computed(() => groupByBucket(visible.value))
const projectColumns = computed(() => groupByProject(visible.value))
const weekColumns = computed(() => groupByWeek(visible.value, now.value))
const today = computed(() => ymdOf(now.value))

// BR-10: shown only while calibration is on (the backend reports it with the board).
const calibration = computed(() => calibrationHint(store.board?.calibration))
const capacity = computed(() => store.board?.capacity ?? null)
/** Why there is no capacity bar: no Google account → connect hint; other errors → retry. */
const capacityError = computed(() => (store.board && !store.board.capacity ? store.board.capacity_error ?? null : null))
const refreshingCalendar = ref(false)
async function refreshCalendar() {
  if (refreshingCalendar.value) return
  refreshingCalendar.value = true
  try {
    await store.refreshCalendar()
  } finally {
    refreshingCalendar.value = false
  }
}
const overloadMin = computed(() => (capacity.value ? capacity.value.need_min - capacity.value.free_min : 0))
const capacityPct = computed(() => {
  const c = capacity.value
  if (!c) return 0
  return c.free_min <= 0 ? 100 : Math.min(100, Math.round((c.need_min / c.free_min) * 100))
})

function bucketEstimate(items: { effective_estimate_min: number }[]): string {
  return formatEstimate(items.reduce((n, it) => n + it.effective_estimate_min, 0))
}

function projectName(id: string | null): string {
  if (!id) return t('focus.columns.noProject')
  return projects.projects.find((p) => p.id === id)?.name ?? id
}
function chipStyle(id: string | null) {
  if (!id) return {}
  const h = projectHue(id)
  return { backgroundColor: `hsl(${h} 70% 50%)` }
}

function weekdayLabel(ymd: string): string {
  const [y, m, d] = ymd.split('-').map(Number)
  const date = new Date(y, m - 1, d)
  return `${date.toLocaleDateString(locale.value, { weekday: 'short' })} ${String(d).padStart(2, '0')}/${String(m).padStart(2, '0')}`
}
/** Overload is only known for today, and only with Calendar data (BR-08). */
function dayOverloaded(date: string): boolean {
  return date === today.value && !!capacity.value && overloadMin.value > 0
}

// ── Drawer ──
const openId = ref<string | null>(null)
const openItem = computed(() => (openId.value ? store.items.find((it) => it.todo.id === openId.value) ?? null : null))
// Deleted elsewhere → close.
watch(openItem, (it) => {
  if (!it && openId.value && store.loaded) openId.value = null
})

// ── Reminder clicks → open + flash the todo ──
const flashId = ref<string | null>(null)
let flashTimer: ReturnType<typeof setTimeout> | null = null
async function showTodo(id: string | null | undefined) {
  if (!id) return
  if (!store.loaded) await store.load()
  if (!store.items.some((it) => it.todo.id === id)) return
  openId.value = id
  // Make sure its bucket is visible in the time view, then flash the row.
  const it = store.items.find((x) => x.todo.id === id)
  if (it && view.value === 'time' && !openBuckets.value.has(it.bucket)) toggleBucket(it.bucket)
  flashId.value = id
  if (flashTimer) clearTimeout(flashTimer)
  flashTimer = setTimeout(() => (flashId.value = null), 2500)
  await nextTick()
  document.querySelector(`[data-focus-row="${CSS.escape(id)}"]`)?.scrollIntoView({ block: 'center', behavior: 'smooth' })
}

let unlistenOpen: UnlistenFn | null = null
onMounted(async () => {
  void store.start()
  if (!projects.projects.length) void projects.fetchProjects()
  try {
    unlistenOpen = await listen<FocusOpenTodo>(FOCUS_OPEN_TODO, (e) => {
      // Consume the pending id too, so a later window mount doesn't reopen it.
      void invoke('focus_take_open_todo').catch(() => {})
      void showTodo(e.payload?.todo_id)
    })
    void showTodo(await invoke<string | null>('focus_take_open_todo'))
  } catch {
    /* outside the Tauri shell */
  }
})
onBeforeUnmount(() => {
  store.stop()
  unlistenOpen?.()
  if (flashTimer) clearTimeout(flashTimer)
})
</script>

<template>
  <div class="flex h-full flex-col">
    <!-- Header -->
    <div class="flex h-13 shrink-0 items-center justify-between gap-3 border-b border-border/60 px-6">
      <div class="flex min-w-0 items-center gap-2">
        <h1 class="shrink-0 text-sm font-semibold">{{ t('focus.title') }}</h1>
        <span
          v-if="store.loaded && openCount > 0"
          class="flex h-4 min-w-4 shrink-0 items-center justify-center rounded-full bg-muted px-1.5 text-[10px] font-medium text-muted-foreground"
        >
          {{ openCount }}
        </span>
        <span v-if="calibration" class="flex min-w-0 items-center gap-1 truncate text-[11px] text-muted-foreground">
          <Gauge class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
          <span class="truncate">
            {{ calibration > 1 ? t('focus.calibration.slower', { factor: calibration }) : t('focus.calibration.faster', { factor: calibration }) }}
          </span>
        </span>
      </div>
      <div class="flex shrink-0 items-center gap-2">
        <!-- P3: "Plan today ✨" goes here. -->
        <AddTodoButton button-variant="primary" size="sm" />
      </div>
    </div>

    <!-- Today's load (BR-08) — only once the backend has Calendar data -->
    <div v-if="capacity" class="flex shrink-0 items-center gap-3 border-b border-border/60 px-6 py-2 text-xs">
      <span class="shrink-0">{{ t('focus.capacity.summary', { need: formatEstimate(capacity.need_min), free: formatEstimate(Math.max(0, capacity.free_min)) }) }}</span>
      <div class="h-1.5 min-w-24 flex-1 overflow-hidden rounded-full bg-muted">
        <div class="h-full rounded-full" :class="overloadMin > 0 ? 'bg-red-500' : 'bg-emerald-500'" :style="{ width: `${capacityPct}%` }" />
      </div>
      <span v-if="overloadMin > 0" class="flex shrink-0 items-center gap-1 font-medium text-red-600 dark:text-red-400">
        <TriangleAlert class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ t('focus.capacity.overloaded', { over: formatEstimate(overloadMin) }) }}
      </span>
      <span v-if="capacity.busy.length" class="shrink-0 text-muted-foreground" :title="capacity.busy.map((b) => b.title).join('\n')">
        {{ t('focus.capacity.busy', { n: capacity.busy.length }, capacity.busy.length) }}
      </span>
      <Button
        variant="ghost"
        size="icon-sm"
        :disabled="refreshingCalendar"
        :title="t('focus.capacity.refresh')"
        :aria-label="t('focus.capacity.refresh')"
        @click="refreshCalendar"
      >
        <RefreshCw class="h-3.5 w-3.5" :class="refreshingCalendar ? 'animate-spin' : ''" :stroke-width="1.75" />
      </Button>
    </div>
    <div
      v-else-if="capacityError && capacityError !== CAPACITY_LOADING"
      class="flex shrink-0 items-center gap-2 border-b border-border/60 px-6 py-1.5 text-[11px] text-muted-foreground"
    >
      <CalendarClock class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      <span v-if="capacityError === CAPACITY_NO_ACCOUNT" class="truncate">{{ t('focus.capacity.connect') }}</span>
      <span v-else class="truncate" :title="capacityError">{{ t('focus.capacity.error') }}</span>
      <Button
        v-if="capacityError !== CAPACITY_NO_ACCOUNT"
        variant="ghost"
        size="icon-sm"
        :disabled="refreshingCalendar"
        :title="t('focus.capacity.refresh')"
        :aria-label="t('focus.capacity.refresh')"
        @click="refreshCalendar"
      >
        <RefreshCw class="h-3.5 w-3.5" :class="refreshingCalendar ? 'animate-spin' : ''" :stroke-width="1.75" />
      </Button>
    </div>

    <!-- Toolbar: view switcher + filters -->
    <div class="flex shrink-0 flex-wrap items-center gap-3 border-b border-border/60 px-6 py-2">
      <div class="flex h-7 overflow-hidden rounded-md border border-border" role="tablist" :aria-label="t('focus.views.label')">
        <button
          v-for="v in VIEWS"
          :key="v"
          type="button"
          role="tab"
          :aria-selected="view === v"
          class="cursor-pointer px-3 text-xs transition-colors not-first:border-l not-first:border-border"
          :class="view === v ? 'bg-accent font-medium text-foreground' : 'text-muted-foreground hover:bg-accent/50 hover:text-foreground'"
          @click="view = v"
        >
          {{ t(`focus.views.${v}`) }}
        </button>
      </div>
      <div class="ml-auto flex items-center gap-2">
        <div class="w-44">
          <AppSelect v-model="filter.projectId" :options="projectOptions" size="xs" variant="outline" />
        </div>
        <div class="w-36">
          <AppSelect v-model="filter.sourceType" :options="sourceOptions" size="xs" variant="outline" />
        </div>
      </div>
    </div>

    <!-- First load -->
    <div v-if="!store.loaded && store.loading" class="space-y-3 p-6">
      <Skeleton class="h-24 w-full rounded-lg" />
      <Skeleton v-for="i in 6" :key="i" class="h-8 w-full rounded" />
    </div>

    <!-- Error before anything loaded -->
    <div v-else-if="store.error && !store.loaded" class="p-6">
      <Card class="border-destructive/30" body-class="flex items-center gap-3 p-4">
        <p class="flex-1 text-sm text-destructive">{{ t('focus.loadError') }}: {{ store.error }}</p>
        <Button variant="outline" size="sm" @click="store.load()">{{ t('focus.retry') }}</Button>
      </Card>
    </div>

    <!-- Empty -->
    <div v-else-if="store.items.length === 0" class="flex min-h-80 flex-1 flex-col items-center justify-center text-center">
      <div class="mb-4 flex h-12 w-12 items-center justify-center rounded-xl bg-muted">
        <Target class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
      </div>
      <p class="text-sm font-medium">{{ t('focus.empty.title') }}</p>
      <p class="mt-1 max-w-96 text-xs text-muted-foreground">{{ t('focus.empty.hint') }}</p>
    </div>

    <p v-else-if="visible.length === 0" class="py-12 text-center text-sm text-muted-foreground">{{ t('focus.empty.filtered') }}</p>

    <!-- By time -->
    <div v-else-if="view === 'time'" class="min-h-0 flex-1 space-y-4 overflow-y-auto px-6 py-4">
      <Card v-if="top.length" class="border-primary/30" body-class="p-2">
        <h2 class="flex items-center gap-1.5 px-2 pb-1 pt-0.5 text-[11px] font-semibold uppercase tracking-wide text-primary">
          <Target class="h-3.5 w-3.5" :stroke-width="2" />
          {{ t('focus.nowSection') }}
        </h2>
        <FocusTodoRow
          :data-focus-row="it.todo.id"
          :class="flashId === it.todo.id ? 'ring-2 ring-primary/70' : ''"
          v-for="(it, i) in top"
          :key="it.todo.id"
          :item="it"
          :now="now"
          :rank="i + 1"
          @open="openId = $event"
        />
      </Card>

      <section v-for="g in buckets" :key="g.bucket">
        <button
          type="button"
          class="flex w-full cursor-pointer items-center gap-1.5 rounded px-1 py-1 text-left text-xs font-semibold transition-colors hover:bg-accent/40"
          :class="g.bucket === 'overdue' ? 'text-red-600 dark:text-red-400' : 'text-foreground'"
          :aria-expanded="openBuckets.has(g.bucket)"
          @click="toggleBucket(g.bucket)"
        >
          <component :is="openBuckets.has(g.bucket) ? ChevronDown : ChevronRight" class="h-3.5 w-3.5 shrink-0" :stroke-width="2" />
          {{ t(`focus.buckets.${g.bucket}`) }}
          <span class="font-normal text-muted-foreground">({{ g.items.length }})</span>
          <span v-if="isActiveBucket(g.bucket)" class="ml-auto font-normal text-muted-foreground tabular-nums">~{{ bucketEstimate(g.items) }}</span>
        </button>
        <div v-if="openBuckets.has(g.bucket)" class="mt-0.5">
          <FocusTodoRow
          :data-focus-row="it.todo.id"
          :class="flashId === it.todo.id ? 'ring-2 ring-primary/70' : ''" v-for="it in g.items" :key="it.todo.id" :item="it" :now="now" @open="openId = $event" />
        </div>
      </section>
    </div>

    <!-- By project -->
    <div v-else-if="view === 'project'" class="flex min-h-0 flex-1 gap-3 overflow-x-auto px-6 py-4">
      <div
        v-for="col in projectColumns"
        :key="col.projectId ?? '__none'"
        class="flex w-72 shrink-0 flex-col rounded-lg border border-border/60 bg-muted/20"
      >
        <div class="flex shrink-0 items-center gap-2 border-b border-border/60 px-3 py-2">
          <span v-if="col.projectId" class="h-2.5 w-2.5 shrink-0 rounded-full" :style="chipStyle(col.projectId)" />
          <span class="min-w-0 flex-1 truncate text-xs font-semibold">{{ projectName(col.projectId) }}</span>
          <span class="shrink-0 text-[11px] tabular-nums text-muted-foreground">~{{ formatEstimate(col.estimateMin) }}</span>
          <span v-if="col.overdue" class="shrink-0 text-[11px] font-medium text-red-600 dark:text-red-400">
            {{ t('focus.columns.overdue', { n: col.overdue }) }}
          </span>
        </div>
        <div class="min-h-0 flex-1 space-y-1.5 overflow-y-auto p-2">
          <FocusTodoRow
          :data-focus-row="it.todo.id"
          :class="flashId === it.todo.id ? 'ring-2 ring-primary/70' : ''"
            v-for="it in col.items"
            :key="it.todo.id"
            :item="it"
            :now="now"
            layout="card"
            hide-project
            @open="openId = $event"
          />
        </div>
      </div>
    </div>

    <!-- Week -->
    <div v-else class="min-h-0 flex-1 overflow-x-auto px-6 py-4">
      <div class="grid h-full min-w-[56rem] grid-cols-7 gap-2">
        <div
          v-for="col in weekColumns"
          :key="col.date"
          class="flex min-w-0 flex-col rounded-lg border"
          :class="[
            dayOverloaded(col.date) ? 'border-red-500/50 bg-red-500/5' : 'border-border/60 bg-muted/20',
            col.date === today ? 'ring-1 ring-primary/40' : '',
          ]"
        >
          <div class="flex shrink-0 items-center gap-1 border-b border-border/60 px-2 py-1.5">
            <span class="flex-1 truncate text-[11px] font-semibold" :class="col.date === today ? 'text-primary' : ''">{{ weekdayLabel(col.date) }}</span>
            <span v-if="col.estimateMin" class="text-[10px] tabular-nums text-muted-foreground" :title="t('focus.week.estimateHint')">
              ~{{ formatEstimate(col.estimateMin) }}
            </span>
          </div>
          <p v-if="dayOverloaded(col.date)" class="px-2 pt-1 text-[10px] font-medium text-red-600 dark:text-red-400">
            {{ t('focus.week.overloaded') }} · {{ span(overloadMin) }}
          </p>
          <div class="min-h-0 flex-1 space-y-1.5 overflow-y-auto p-1.5">
            <FocusTodoRow
          :data-focus-row="it.todo.id"
          :class="flashId === it.todo.id ? 'ring-2 ring-primary/70' : ''" v-for="it in col.items" :key="it.todo.id" :item="it" :now="now" layout="card" @open="openId = $event" />
          </div>
        </div>
      </div>
    </div>

    <FocusTodoDrawer :item="openItem" @close="openId = null" />
    <FocusDialogs />
  </div>
</template>
