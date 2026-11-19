<script setup lang="ts">
// Bento tile "Sessions", compact: a donut of the session states with the total
// in its centre, one legend that doubles as the quick filters (a state, plus
// "context > 80%"; each entry and donut segment filters the list) in two
// columns, and the combined activity of every session over the last 20
// minutes — only its rate, next to the title, when the tile is narrow.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Activity } from 'lucide-vue-next'
import { STATE_FILTER, type QuickFilter, type SessionState } from '@/lib/runtimeDashboard'
import { donutSegments } from '@/lib/runtimeCharts'
import AreaChart from './AreaChart.vue'
import BentoCard from './BentoCard.vue'
import CountUp from './CountUp.vue'
import { STATE_FILL, useRuntimeLabels } from './runtimeStyles'

const props = defineProps<{
  total: number
  counts: Record<QuickFilter, number>
  filter: QuickFilter | null
  /** Events per minute of all sessions together, oldest first. */
  activity: number[]
}>()
const emit = defineEmits<{ filter: [f: QuickFilter] }>()
const { t } = useI18n()
const L = useRuntimeLabels()

// ── Donut ────────────────────────────────────────────────────────────────────
const STATES: SessionState[] = ['running', 'waiting', 'quiet', 'idle', 'external']
const SEG_STROKE: Record<SessionState, string> = {
  running: 'stroke-success',
  waiting: 'stroke-warning',
  quiet: 'stroke-warning/55',
  idle: 'stroke-muted-foreground/45',
  external: 'stroke-info',
}
const R = 50
const segments = computed(() =>
  donutSegments(STATES.map((key) => ({ key, n: props.counts[STATE_FILTER[key]] })), R, 3),
)
const donutLabel = computed(() =>
  t('dashboard.stat.sessions.donutLabel', {
    parts: segments.value.map((s) => `${s.n} ${L.stateLabel(s.key)}`).join(', '),
  }),
)
/** A state filter is on: the other segments step back. */
function dimmed(state: SessionState): boolean {
  return props.filter !== null && props.filter !== 'ctx' && props.filter !== STATE_FILTER[state]
}

// ── Legend (= quick filters) ─────────────────────────────────────────────────
// Only what is there, plus the active filter so it can always be cleared.
const legend = computed(() => {
  const rows: { key: QuickFilter; dot: string; square?: boolean }[] = STATES.map((s) => ({ key: STATE_FILTER[s], dot: STATE_FILL[s] }))
  rows.push({ key: 'ctx', dot: 'bg-warning', square: true })
  return rows
    .filter((r) => props.counts[r.key] > 0 || props.filter === r.key)
    .map((r) => ({ ...r, n: props.counts[r.key] }))
})

const rate = computed(() => props.activity[props.activity.length - 1] ?? 0)
</script>

<template>
  <BentoCard :title="t('dashboard.stat.sessions.title')" :icon="Activity" tone="primary" compact>
    <!-- A narrow tile drops the chart; its rate moves up next to the title. -->
    <template #aside>
      <span class="ml-auto hidden shrink-0 font-mono text-[10px] font-medium text-primary tabular-nums @max-[400px]:inline">
        {{ rate }}{{ t('dashboard.stat.sessions.perMinute') }}
      </span>
    </template>
    <div class="flex min-h-0 min-w-0 flex-1 items-center gap-3">
      <div class="relative h-[76px] w-[76px] shrink-0">
        <svg viewBox="0 0 120 120" class="h-full w-full" role="group" :aria-label="donutLabel">
          <circle cx="60" cy="60" :r="R" fill="none" class="stroke-muted" stroke-width="13" />
          <circle
            v-for="seg in segments"
            :key="seg.key"
            cx="60"
            cy="60"
            :r="R"
            fill="none"
            stroke-width="13"
            :stroke-dasharray="seg.dash"
            :stroke-dashoffset="seg.offset"
            transform="rotate(-90 60 60)"
            class="rt-grow cursor-pointer transition-opacity outline-none focus-visible:opacity-70 motion-reduce:transition-none"
            :class="[SEG_STROKE[seg.key], dimmed(seg.key) && 'opacity-30']"
            role="button"
            tabindex="0"
            :aria-pressed="filter === STATE_FILTER[seg.key]"
            :aria-label="t(`dashboard.stat.sessions.chip.${STATE_FILTER[seg.key]}`, { n: seg.n })"
            @click="emit('filter', STATE_FILTER[seg.key])"
            @keydown.enter.prevent="emit('filter', STATE_FILTER[seg.key])"
            @keydown.space.prevent="emit('filter', STATE_FILTER[seg.key])"
          >
            <title>{{ t(`dashboard.stat.sessions.chip.${STATE_FILTER[seg.key]}`, { n: seg.n }) }}</title>
          </circle>
        </svg>
        <div class="pointer-events-none absolute inset-0 grid place-items-center">
          <CountUp :value="total" class="text-2xl leading-none font-extrabold tracking-tight" />
        </div>
      </div>

      <!-- Two columns: at most 6 entries (5 states + context), so 3 rows always fit. -->
      <ul class="grid min-w-0 flex-1 grid-cols-2 content-center gap-x-1.5 gap-y-px" :aria-label="t('dashboard.stat.sessions.legendLabel')">
        <li v-for="r in legend" :key="r.key" class="min-w-0">
          <button
            type="button"
            class="flex w-full min-w-0 cursor-pointer items-center gap-1.5 rounded px-1.5 py-0.5 text-left text-[11px] transition-colors motion-reduce:transition-none"
            :class="filter === r.key ? 'bg-primary/15 font-semibold text-primary' : 'text-foreground hover:bg-accent/60'"
            :aria-pressed="filter === r.key"
            :title="`${t(`dashboard.stat.sessions.legend.${r.key}`)}: ${r.n}`"
            @click="emit('filter', r.key)"
          >
            <span class="h-[7px] w-[7px] shrink-0" :class="[r.dot, r.square ? 'rounded-[2px]' : 'rounded-full']" aria-hidden="true" />
            <span class="min-w-0 flex-1 truncate">{{ t(`dashboard.stat.sessions.legend.${r.key}`) }}</span>
            <span class="font-bold tabular-nums">{{ r.n }}</span>
          </button>
        </li>
        <li v-if="!legend.length" class="col-span-2 px-1.5 text-[11px] text-muted-foreground">{{ t('dashboard.stat.sessions.none') }}</li>
      </ul>

      <div class="flex w-[26%] max-w-[130px] min-w-[64px] shrink-0 flex-col gap-0.5 @max-[400px]:hidden">
        <span class="truncate font-mono text-[10px] text-muted-foreground">{{ t('dashboard.stat.sessions.activityShort') }}</span>
        <AreaChart :values="activity" :height="34" :floor="5" tone="primary" dot :label="t('dashboard.stat.sessions.activityLabel')" />
        <span class="text-right text-[13px] font-bold text-primary tabular-nums">
          {{ rate }}<span class="text-[10px] font-medium text-muted-foreground">{{ t('dashboard.stat.sessions.perMinute') }}</span>
        </span>
      </div>
    </div>
  </BentoCard>
</template>
