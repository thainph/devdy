<script setup lang="ts">
/**
 * The ranked list of the phone's Focus screen, in one of three views:
 *  - Time: "Do now" (the Host's top 3) as cards with Done / Start right on
 *    them, then the time buckets without those three — In progress / Overdue /
 *    Today open, the rest folded;
 *  - Project: one section per project with open work, ordered by its best todo;
 *  - Week: Monday → Sunday of this week, todos on their due day.
 *
 * Ranking and bucketing come from the Host; this only filters and renders.
 * A row's circle marks it done; tapping a row opens its action sheet.
 */
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Check, ChevronDown, ChevronRight, Pause, Play } from 'lucide-vue-next'
import {
  DEFAULT_OPEN_BUCKETS,
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
} from '@/lib/focus'
import { todoAiKey } from '@/lib/remoteFocus'
import { splitTodoText } from '@/lib/remoteTodos'
import { useFocusText } from '@/components/focus/useFocusText'
import FocusBoardRow from './FocusBoardRow.vue'
import type { TodoPatchWire } from '../protocol'

export type FocusBoardView = 'time' | 'project' | 'week'

const props = defineProps<{
  board: FocusBoard
  /** The board's open rows after the screen's project filter. */
  items: FocusItem[]
  projectNames: Map<string, string>
  /** Epoch ms, ticking, for the relative labels. */
  now: number
  view: FocusBoardView
  /** Running AI requests by key (`todo:<id>` = an analysis). */
  aiRunning: Record<string, string>
}>()

const emit = defineEmits<{
  toggle: [id: string]
  actions: [item: FocusItem]
  patch: [id: string, patch: TodoPatchWire]
}>()

const { t, locale } = useI18n()
const ft = useFocusText()

const top = computed(() => topItems(props.items, props.board.top))
// "Do now" already shows these three: the buckets skip them.
const rest = computed(() => {
  const shown = new Set(top.value.map((it) => it.todo.id))
  return props.items.filter((it) => !shown.has(it.todo.id))
})
const groups = computed(() => groupByBucket(rest.value))
const projectColumns = computed(() => groupByProject(props.items))
const weekColumns = computed(() => groupByWeek(props.items, props.now))
const today = computed(() => ymdOf(props.now))

const expanded = ref(new Set<FocusBucket>(DEFAULT_OPEN_BUCKETS))
function flip(b: FocusBucket): void {
  const next = new Set(expanded.value)
  if (next.has(b)) next.delete(b)
  else next.add(b)
  expanded.value = next
}

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
/** A "Do now" card's one line of facts: due (with its tone) · estimate · project. */
function cardMeta(it: FocusItem): { due: { text: string; tone: string } | null; rest: string } {
  const parts = [`~${formatEstimate(it.effective_estimate_min)}${it.estimate_missing ? '?' : ''}`]
  const p = it.todo.project_id ? props.projectNames.get(it.todo.project_id) : null
  if (p) parts.push(p)
  return { due: ft.due(it, props.now), rest: parts.join(' · ') }
}

const heading = 'mb-1.5 px-1 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground'
const card = 'ui-card overflow-hidden rounded-xl border border-border bg-card'
const TONE: Record<string, string> = { danger: 'text-destructive font-medium', warn: 'text-warning font-medium', muted: '' }
const actionBtn =
  'flex h-9 flex-1 items-center justify-center gap-1.5 rounded-lg text-[13px] font-semibold cursor-pointer transition-colors'
</script>

<template>
  <div>
    <!-- By time -->
    <template v-if="view === 'time'">
      <section v-if="top.length" class="mb-4">
        <h2 :class="heading" class="!text-primary">{{ t('focus.nowSection') }}</h2>
        <div class="flex flex-col gap-2">
          <div
            v-for="it in top"
            :key="it.todo.id"
            class="ui-card rounded-xl border bg-card px-3 pt-2.5 pb-2.5 cursor-pointer active:bg-accent/40"
            :class="it.todo.status === 'doing' ? 'border-primary/60' : 'border-border'"
            @click="emit('actions', it)"
          >
            <p class="text-[15px] leading-snug font-semibold break-words">
              <span
                v-if="it.todo.priority === 1"
                class="mr-1 rounded bg-destructive/15 px-1 align-[1px] text-[10px] font-bold text-destructive"
              >P1</span>
              <Play v-if="it.todo.status === 'doing'" class="mr-0.5 inline h-3.5 w-3.5 text-primary" :stroke-width="2.5" />
              {{ splitTodoText(it.todo.text).title }}
            </p>
            <p class="mt-0.5 text-xs text-muted-foreground">
              <template v-if="cardMeta(it).due">
                <span :class="TONE[cardMeta(it).due!.tone]">{{ cardMeta(it).due!.text }}</span> ·
              </template>
              {{ cardMeta(it).rest }}
            </p>
            <div class="mt-2.5 flex gap-2">
              <button
                type="button"
                :class="actionBtn"
                class="bg-success/15 text-success active:bg-success/25"
                @click.stop="emit('toggle', it.todo.id)"
              >
                <Check class="h-4 w-4" :stroke-width="2.5" />
                {{ t('focus.actions.done') }}
              </button>
              <button
                v-if="it.todo.status !== 'doing'"
                type="button"
                :class="actionBtn"
                class="bg-primary/15 text-primary active:bg-primary/25"
                @click.stop="emit('patch', it.todo.id, { status: 'doing', snoozed_until: null })"
              >
                <Play class="h-4 w-4" :stroke-width="2.5" />
                {{ t('focus.actions.start') }}
              </button>
              <button
                v-else
                type="button"
                :class="actionBtn"
                class="bg-secondary text-foreground/80 active:bg-accent"
                @click.stop="emit('patch', it.todo.id, { status: 'todo' })"
              >
                <Pause class="h-4 w-4" :stroke-width="2.5" />
                {{ t('controller.focus.pause') }}
              </button>
            </div>
          </div>
        </div>
      </section>

      <section v-for="g in groups" :key="g.bucket" class="mb-3">
        <button
          type="button"
          class="flex w-full items-center gap-1 py-1 cursor-pointer"
          :class="heading"
          :aria-expanded="expanded.has(g.bucket)"
          @click="flip(g.bucket)"
        >
          <component :is="expanded.has(g.bucket) ? ChevronDown : ChevronRight" class="h-3.5 w-3.5" :stroke-width="2.5" />
          {{ t(`focus.buckets.${g.bucket}`) }} · {{ g.items.length }}
          <span v-if="isActiveBucket(g.bucket)" class="ml-auto normal-case tracking-normal font-normal tabular-nums">
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
          <span class="shrink-0 normal-case tracking-normal font-normal tabular-nums">~{{ formatEstimate(col.estimateMin) }}</span>
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
            @actions="emit('actions', $event)"
          />
        </div>
      </section>
    </template>

    <!-- Week -->
    <template v-else>
      <section v-for="col in weekColumns" :key="col.date" class="mb-3">
        <h2 :class="[heading, col.date === today ? '!text-primary' : '']" class="flex items-center gap-1.5">
          <span class="min-w-0 flex-1 truncate">{{ weekdayLabel(col.date) }}</span>
          <span v-if="col.estimateMin" class="shrink-0 normal-case tracking-normal font-normal tabular-nums">
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
            @actions="emit('actions', $event)"
          />
        </div>
        <p v-else class="px-1 text-xs text-muted-foreground/70">{{ t('controller.focus.week.nothingDue') }}</p>
      </section>
    </template>
  </div>
</template>
