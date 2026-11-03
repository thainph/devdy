<script setup lang="ts">
/**
 * The open half of the phone's Todo screen when the Host ranks todos (Focus
 * board, `docs/focus-board-spec.md` §3.2): today's load against the Calendar,
 * "Do now" (the Host's top 3), then the time buckets — In progress / Overdue /
 * Today open, the rest folded.
 *
 * Ranking and bucketing come from the Host; this only filters and renders.
 * A row's circle marks it done, the row opens the editor, `⋯` the quick
 * actions (start, wait, snooze…).
 */
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { ChevronDown, ChevronRight, Hourglass, MoreHorizontal, Play } from 'lucide-vue-next'
import {
  CAPACITY_NO_ACCOUNT,
  DEFAULT_OPEN_BUCKETS,
  formatEstimate,
  groupByBucket,
  topItems,
  type FocusBoard,
  type FocusBucket,
  type FocusItem,
  type FocusTodo,
} from '@/lib/focus'
import { splitTodoText } from '@/lib/remoteTodos'
import { useFocusText } from '@/components/focus/useFocusText'

const props = defineProps<{
  board: FocusBoard
  /** The board's rows after the screen's project filter and search. */
  items: FocusItem[]
  projectNames: Map<string, string>
  /** Epoch ms, ticking, for the relative labels. */
  now: number
}>()

const emit = defineEmits<{
  toggle: [id: string]
  open: [todo: FocusTodo]
  actions: [item: FocusItem]
}>()

const { t } = useI18n()
const ft = useFocusText()

const open = computed(() => props.items.filter((it) => it.bucket !== 'done'))
const top = computed(() => topItems(open.value, props.board.top))
const groups = computed(() => groupByBucket(open.value))

const expanded = ref(new Set<FocusBucket>(DEFAULT_OPEN_BUCKETS))
function flip(b: FocusBucket): void {
  const next = new Set(expanded.value)
  if (next.has(b)) next.delete(b)
  else next.add(b)
  expanded.value = next
}

const capacity = computed(() => {
  const c = props.board.capacity
  if (!c) return null
  const over = c.need_min - c.free_min
  return {
    text: t('focus.capacity.summary', { need: ft.span(c.need_min), free: ft.span(Math.max(0, c.free_min)) }),
    over: over > 0 ? t('focus.capacity.overloaded', { over: ft.span(over) }) : null,
  }
})
const capacityHint = computed(() => {
  const e = props.board.capacity_error
  if (props.board.capacity || !e || e === 'loading') return null
  return e === CAPACITY_NO_ACCOUNT ? t('focus.capacity.connect') : t('focus.capacity.error')
})

const TONES: Record<string, string> = {
  danger: 'border-destructive/40 bg-destructive/10 text-destructive',
  warn: 'border-warning/40 bg-warning/10 text-warning',
  muted: 'border-border bg-secondary text-muted-foreground',
}
const chip = 'rounded-full border px-1.5 py-px text-[10px]'
const heading = 'mb-1.5 px-0.5 text-[10px] font-medium uppercase tracking-wider text-muted-foreground'
</script>

<template>
  <div>
    <div
      v-if="capacity || capacityHint"
      class="mb-3 rounded-xl border px-3 py-2 text-xs"
      :class="capacity?.over ? 'border-destructive/40 bg-destructive/10' : 'border-border bg-card'"
    >
      <template v-if="capacity">
        <span>{{ capacity.text }}</span>
        <span v-if="capacity.over" class="font-semibold text-destructive"> · {{ capacity.over }}</span>
      </template>
      <span v-else class="text-muted-foreground">{{ capacityHint }}</span>
    </div>

    <section v-if="top.length" class="mb-3">
      <h2 :class="heading" class="!text-primary">{{ t('focus.nowSection') }}</h2>
      <div class="ui-card overflow-hidden rounded-xl border border-primary/40 bg-card">
        <template v-for="(it, i) in top" :key="it.todo.id">
          <div
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
        </template>
      </div>
    </section>

    <section v-for="g in groups" :key="g.bucket" class="mb-3">
      <button
        type="button"
        class="flex w-full items-center gap-1 cursor-pointer"
        :class="heading"
        @click="flip(g.bucket)"
      >
        <component :is="expanded.has(g.bucket) ? ChevronDown : ChevronRight" class="h-3 w-3" :stroke-width="2.5" />
        {{ t(`focus.buckets.${g.bucket}`) }} · {{ g.items.length }}
      </button>
      <div v-if="expanded.has(g.bucket)" class="ui-card overflow-hidden rounded-xl border border-border bg-card">
        <div
          v-for="it in g.items"
          :key="it.todo.id"
          class="flex items-start gap-2.5 px-3 py-2.5 border-b border-border/60 last:border-b-0 cursor-pointer hover:bg-accent/50"
          @click="emit('open', it.todo)"
        >
          <button
            class="mt-0.5 h-5 w-5 shrink-0 rounded-full border-2 border-foreground/30 transition-colors hover:border-success cursor-pointer"
            :title="t('controller.todo.markDone')"
            :aria-label="t('controller.todo.markDone')"
            @click.stop="emit('toggle', it.todo.id)"
          />
          <div class="min-w-0 flex-1">
            <p class="text-sm font-medium break-words">
              <Play v-if="it.todo.status === 'doing'" class="mr-0.5 inline h-3 w-3 text-primary" :stroke-width="2.5" />
              <Hourglass
                v-else-if="it.todo.status === 'waiting'"
                class="mr-0.5 inline h-3 w-3 text-muted-foreground"
                :stroke-width="2.5"
              />
              {{ splitTodoText(it.todo.text).title }}
            </p>
            <div class="mt-1 flex flex-wrap gap-1">
              <span v-if="it.todo.priority === 1" :class="[chip, TONES.danger]" class="font-semibold">P1</span>
              <span v-if="ft.due(it, now)" :class="[chip, TONES[ft.due(it, now)!.tone]]">
                {{ ft.due(it, now)!.text }}
              </span>
              <span v-if="it.bucket === 'snoozed' && it.todo.snoozed_until" :class="[chip, TONES.muted]">
                {{ t('focus.snooze.until', { time: ft.stamp(it.todo.snoozed_until) }) }}
              </span>
              <span :class="[chip, TONES.muted]">
                ~{{ formatEstimate(it.effective_estimate_min) }}<template v-if="it.estimate_missing">?</template>
              </span>
              <span
                v-if="it.todo.project_id && projectNames.get(it.todo.project_id)"
                :class="[chip, TONES.muted]"
              >
                {{ projectNames.get(it.todo.project_id) }}
              </span>
            </div>
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
  </div>
</template>
