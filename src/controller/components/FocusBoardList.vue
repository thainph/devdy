<script setup lang="ts">
/**
 * The open half of the phone's Todo screen when the Host ranks todos (Focus
 * board, `docs/focus-board-spec.md` §3.2): today's load against the Calendar
 * (with a refresh), the calibration hint, then one of three views —
 *  - Time: "Do now" (the Host's top 3), then the time buckets — In progress /
 *    Overdue / Today open, the rest folded;
 *  - Project: one section per project with open work, ordered by its best todo;
 *  - Week: Monday → Sunday of this week, todos on their due day.
 *
 * Ranking and bucketing come from the Host; this only filters and renders.
 * A row's circle marks it done, the row opens the editor, `⋯` the quick
 * actions (start, wait, snooze, AI…).
 */
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { ChevronDown, ChevronRight, Gauge, MoreHorizontal, Play, RefreshCw } from 'lucide-vue-next'
import {
  CAPACITY_LOADING,
  CAPACITY_NO_ACCOUNT,
  DEFAULT_OPEN_BUCKETS,
  calibrationHint,
  formatEstimate,
  groupByBucket,
  groupByProject,
  groupByWeek,
  isActiveBucket,
  topItems,
  ymdOf,
  type FocusBoard,
  type FocusBucket,
  type FocusItem,
  type FocusTodo,
} from '@/lib/focus'
import { todoAiKey } from '@/lib/remoteFocus'
import { splitTodoText } from '@/lib/remoteTodos'
import { useFocusText } from '@/components/focus/useFocusText'
import FocusBoardRow from './FocusBoardRow.vue'

export type FocusBoardView = 'time' | 'project' | 'week'

const props = defineProps<{
  board: FocusBoard
  /** The board's rows after the screen's project filter and search. */
  items: FocusItem[]
  projectNames: Map<string, string>
  /** Epoch ms, ticking, for the relative labels. */
  now: number
  view: FocusBoardView
  /** Running AI requests by key (`todo:<id>` = an analysis). */
  aiRunning: Record<string, string>
  /** A Calendar refresh is in flight. */
  calendarRefreshing?: boolean
}>()

const emit = defineEmits<{
  toggle: [id: string]
  open: [todo: FocusTodo]
  actions: [item: FocusItem]
  'refresh-calendar': []
}>()

const { t, locale } = useI18n()
const ft = useFocusText()

const open = computed(() => props.items.filter((it) => it.bucket !== 'done'))
const top = computed(() => topItems(open.value, props.board.top))
const groups = computed(() => groupByBucket(open.value))
const projectColumns = computed(() => groupByProject(open.value))
const weekColumns = computed(() => groupByWeek(open.value, props.now))
const today = computed(() => ymdOf(props.now))

const expanded = ref(new Set<FocusBucket>(DEFAULT_OPEN_BUCKETS))
function flip(b: FocusBucket): void {
  const next = new Set(expanded.value)
  if (next.has(b)) next.delete(b)
  else next.add(b)
  expanded.value = next
}

const overloadMin = computed(() => {
  const c = props.board.capacity
  return c ? c.need_min - c.free_min : 0
})
const capacity = computed(() => {
  const c = props.board.capacity
  if (!c) return null
  return {
    text: t('focus.capacity.summary', { need: ft.span(c.need_min), free: ft.span(Math.max(0, c.free_min)) }),
    over: overloadMin.value > 0 ? t('focus.capacity.overloaded', { over: ft.span(overloadMin.value) }) : null,
  }
})
const capacityHint = computed(() => {
  const e = props.board.capacity_error
  if (props.board.capacity || !e || e === CAPACITY_LOADING) return null
  return e === CAPACITY_NO_ACCOUNT ? t('focus.capacity.connect') : t('focus.capacity.error')
})
/** Refresh is offered wherever there is a Calendar to re-read. */
const canRefresh = computed(() => props.board.capacity_error !== CAPACITY_NO_ACCOUNT)
const calibration = computed(() => calibrationHint(props.board.calibration))

const analyzing = (id: string) => !!props.aiRunning[todoAiKey(id)]

function projectName(id: string | null): string {
  if (!id) return t('focus.columns.noProject')
  return props.projectNames.get(id) ?? id
}
function weekdayLabel(ymd: string): string {
  const [y, m, d] = ymd.split('-').map(Number)
  const date = new Date(y, m - 1, d)
  return `${date.toLocaleDateString(locale.value, { weekday: 'short' })} ${String(d).padStart(2, '0')}/${String(m).padStart(2, '0')}`
}
function bucketEstimate(items: FocusItem[]): string {
  return formatEstimate(items.reduce((n, it) => n + it.effective_estimate_min, 0))
}

const heading = 'mb-1.5 px-0.5 text-[10px] font-medium uppercase tracking-wider text-muted-foreground'
const card = 'ui-card overflow-hidden rounded-xl border border-border bg-card'
</script>

<template>
  <div>
    <div
      v-if="capacity || capacityHint"
      class="mb-2 flex items-center gap-2 rounded-xl border px-3 py-2 text-xs"
      :class="capacity?.over ? 'border-destructive/40 bg-destructive/10' : 'border-border bg-card'"
    >
      <p class="min-w-0 flex-1">
        <template v-if="capacity">
          <span>{{ capacity.text }}</span>
          <span v-if="capacity.over" class="font-semibold text-destructive"> · {{ capacity.over }}</span>
        </template>
        <span v-else class="text-muted-foreground">{{ capacityHint }}</span>
      </p>
      <button
        v-if="canRefresh"
        type="button"
        class="-my-1 -mr-1.5 grid h-7 w-7 shrink-0 place-items-center rounded-md text-muted-foreground cursor-pointer hover:bg-accent disabled:opacity-50"
        :disabled="calendarRefreshing"
        :title="t('focus.capacity.refresh')"
        :aria-label="t('focus.capacity.refresh')"
        @click="emit('refresh-calendar')"
      >
        <RefreshCw class="h-3.5 w-3.5" :class="calendarRefreshing ? 'animate-spin' : ''" :stroke-width="2" />
      </button>
    </div>
    <p v-if="calibration" class="mb-3 flex items-center gap-1.5 px-0.5 text-[11px] text-muted-foreground">
      <Gauge class="h-3.5 w-3.5 shrink-0" :stroke-width="2" />
      {{
        calibration > 1
          ? t('focus.calibration.slower', { factor: calibration })
          : t('focus.calibration.faster', { factor: calibration })
      }}
    </p>

    <!-- By time -->
    <template v-if="view === 'time'">
      <section v-if="top.length" class="mb-3">
        <h2 :class="heading" class="!text-primary">{{ t('focus.nowSection') }}</h2>
        <div class="ui-card overflow-hidden rounded-xl border border-primary/40 bg-card">
          <div
            v-for="(it, i) in top"
            :key="it.todo.id"
            class="flex items-start gap-2.5 px-3 py-2.5 border-b border-border/60 last:border-b-0 cursor-pointer hover:bg-accent/50"
            @click="emit('open', it.todo)"
          >
            <span class="mt-0.5 w-4 shrink-0 text-center text-xs font-semibold text-primary">{{ i + 1 }}</span>
            <button
              class="mt-0.5 h-5 w-5 shrink-0 rounded-full border-2 border-foreground/30 transition-colors hover:border-success cursor-pointer"
              :title="t('controller.todo.markDone')"
              :aria-label="t('controller.todo.markDone')"
              @click.stop="emit('toggle', it.todo.id)"
            />
            <div class="min-w-0 flex-1">
              <p class="text-sm font-medium break-words">
                <Play v-if="it.todo.status === 'doing'" class="mr-0.5 inline h-3 w-3 text-primary" :stroke-width="2.5" />
                {{ splitTodoText(it.todo.text).title }}
              </p>
              <p class="mt-0.5 text-[11px] leading-snug text-muted-foreground">{{ ft.whyText(it, now) }}</p>
            </div>
            <button
              class="-mr-1 grid h-7 w-7 shrink-0 place-items-center rounded-md text-muted-foreground cursor-pointer hover:bg-accent"
              :aria-label="t('focus.whyHere')"
              @click.stop="emit('actions', it)"
            >
              <MoreHorizontal class="h-4 w-4" :stroke-width="2" />
            </button>
          </div>
        </div>
      </section>

      <section v-for="g in groups" :key="g.bucket" class="mb-3">
        <button
          type="button"
          class="flex w-full items-center gap-1 cursor-pointer"
          :class="heading"
          :aria-expanded="expanded.has(g.bucket)"
          @click="flip(g.bucket)"
        >
          <component :is="expanded.has(g.bucket) ? ChevronDown : ChevronRight" class="h-3 w-3" :stroke-width="2.5" />
          {{ t(`focus.buckets.${g.bucket}`) }} · {{ g.items.length }}
          <span v-if="isActiveBucket(g.bucket)" class="ml-auto normal-case tracking-normal tabular-nums">
            ~{{ bucketEstimate(g.items) }}
          </span>
        </button>
        <div v-if="expanded.has(g.bucket)" :class="card">
          <FocusBoardRow
            v-for="it in g.items"
            :key="it.todo.id"
            :item="it"
            :project-names="projectNames"
            :now="now"
            :analyzing="analyzing(it.todo.id)"
            @toggle="emit('toggle', $event)"
            @open="emit('open', $event)"
            @actions="emit('actions', $event)"
          />
        </div>
      </section>
    </template>

    <!-- By project -->
    <template v-else-if="view === 'project'">
      <section v-for="col in projectColumns" :key="col.projectId ?? '__none'" class="mb-3">
        <h2 :class="heading" class="flex items-center gap-1.5">
          <span class="min-w-0 flex-1 truncate">{{ projectName(col.projectId) }} · {{ col.items.length }}</span>
          <span v-if="col.overdue" class="shrink-0 normal-case tracking-normal text-destructive">
            {{ t('focus.columns.overdue', { n: col.overdue }) }}
          </span>
          <span class="shrink-0 normal-case tracking-normal tabular-nums">~{{ formatEstimate(col.estimateMin) }}</span>
        </h2>
        <div :class="card">
          <FocusBoardRow
            v-for="it in col.items"
            :key="it.todo.id"
            :item="it"
            :project-names="projectNames"
            :now="now"
            hide-project
            :analyzing="analyzing(it.todo.id)"
            @toggle="emit('toggle', $event)"
            @open="emit('open', $event)"
            @actions="emit('actions', $event)"
          />
        </div>
      </section>
    </template>

    <!-- Week -->
    <template v-else>
      <section v-for="col in weekColumns" :key="col.date" class="mb-3">
        <h2
          :class="[heading, col.date === today ? '!text-primary' : '']"
          class="flex items-center gap-1.5"
        >
          <span class="min-w-0 flex-1 truncate">{{ weekdayLabel(col.date) }}</span>
          <span
            v-if="col.date === today && capacity?.over"
            class="shrink-0 normal-case tracking-normal text-destructive"
          >
            {{ t('focus.week.overloaded') }} · {{ ft.span(overloadMin) }}
          </span>
          <span v-if="col.estimateMin" class="shrink-0 normal-case tracking-normal tabular-nums">
            ~{{ formatEstimate(col.estimateMin) }}
          </span>
        </h2>
        <div v-if="col.items.length" :class="card">
          <FocusBoardRow
            v-for="it in col.items"
            :key="it.todo.id"
            :item="it"
            :project-names="projectNames"
            :now="now"
            :analyzing="analyzing(it.todo.id)"
            @toggle="emit('toggle', $event)"
            @open="emit('open', $event)"
            @actions="emit('actions', $event)"
          />
        </div>
        <p v-else class="px-0.5 text-[11px] text-muted-foreground/70">{{ t('controller.focus.week.nothingDue') }}</p>
      </section>
    </template>
  </div>
</template>
