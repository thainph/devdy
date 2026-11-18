<script setup lang="ts">
// Bento hero "Sessions": a donut of the session states (a segment filters the
// list like the chips), the total in its centre, the running count, the quick
// filter chips, the combined activity of every session over the last 20
// minutes, and a square per session (click = select it, which also opens its
// group).
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Activity } from 'lucide-vue-next'
import { STATE_FILTER, type QuickFilter, type RtSession, type SessionState } from '@/lib/runtimeDashboard'
import { donutSegments } from '@/lib/runtimeCharts'
import AreaChart from './AreaChart.vue'
import BentoCard from './BentoCard.vue'
import CountUp from './CountUp.vue'
import { STATE_FILL, useRuntimeLabels } from './runtimeStyles'

const props = defineProps<{
  /** Every session, in group order. */
  sessions: RtSession[]
  counts: Record<QuickFilter, number>
  filter: QuickFilter | null
  selectedId: string | null
  /** Events per minute of all sessions together, oldest first. */
  activity: number[]
}>()
const emit = defineEmits<{ pick: [id: string]; filter: [f: QuickFilter] }>()

const { t } = useI18n()
const L = useRuntimeLabels()

const conductors = computed(() => props.sessions.filter((s) => s.role === 'conductor').length)
const external = computed(() => props.sessions.filter((s) => s.role === 'external').length)

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

// ── Chips ────────────────────────────────────────────────────────────────────
const CHIP_DOT: Record<QuickFilter, string> = {
  wait: STATE_FILL.waiting,
  quiet: STATE_FILL.quiet,
  ctx: 'bg-warning',
  run: STATE_FILL.running,
  idle: STATE_FILL.idle,
  ext: STATE_FILL.external,
}
/** The usual chips, plus the active filter's when a donut segment set it, so it can be cleared. */
const chips = computed(() => {
  const keys: QuickFilter[] = ['wait', 'quiet', 'ctx', 'ext']
  const shown = keys.filter((k) => (k !== 'wait' && k !== 'ext') || props.counts[k] > 0)
  if (props.filter && !shown.includes(props.filter)) shown.push(props.filter)
  return shown.map((key) => ({ key, n: props.counts[key], dot: CHIP_DOT[key] }))
})

// ── Activity ─────────────────────────────────────────────────────────────────
const rate = computed(() => props.activity[props.activity.length - 1] ?? 0)

function tip(s: RtSession): string {
  return [s.title, s.projectName, L.stateLabel(s.state)].filter(Boolean).join(' · ')
}
</script>

<template>
  <BentoCard :title="t('dashboard.stat.sessions.title')" :icon="Activity" tone="primary">
    <div class="flex flex-wrap items-center gap-x-6 gap-y-3">
      <div class="relative h-[132px] w-[132px] shrink-0">
        <svg viewBox="0 0 120 120" class="h-full w-full" role="group" :aria-label="donutLabel">
          <circle cx="60" cy="60" :r="R" fill="none" class="stroke-muted" stroke-width="12" />
          <circle
            v-for="seg in segments"
            :key="seg.key"
            cx="60"
            cy="60"
            :r="R"
            fill="none"
            stroke-width="12"
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
        <div class="pointer-events-none absolute inset-0 grid place-items-center text-center">
          <div>
            <CountUp :value="sessions.length" class="block text-4xl leading-none font-extrabold tracking-tight" />
            <span class="text-[11px] font-semibold text-muted-foreground">{{ t('dashboard.stat.sessions.unit', sessions.length) }}</span>
          </div>
        </div>
      </div>

      <div class="flex min-w-[220px] flex-1 flex-col gap-2.5">
        <div class="flex min-w-0 flex-wrap items-baseline gap-x-2.5 gap-y-1">
          <CountUp :value="counts.run" class="text-5xl leading-none font-extrabold tracking-tight" />
          <span class="text-sm font-bold whitespace-nowrap">{{ t('dashboard.stat.sessions.running') }}</span>
          <span class="min-w-0 truncate text-xs text-muted-foreground" :title="t('dashboard.stat.sessions.meta', { conductors, external }, conductors)">· {{ t('dashboard.stat.sessions.meta', { conductors, external }, conductors) }}</span>
        </div>
        <div class="flex flex-wrap gap-1.5">
          <button
            v-for="c in chips"
            :key="c.key"
            type="button"
            class="inline-flex min-h-7 cursor-pointer items-center gap-1.5 rounded-full border px-2.5 text-xs font-semibold transition-colors motion-reduce:transition-none"
            :class="filter === c.key ? 'border-primary bg-primary/15 text-primary' : 'border-border bg-card text-foreground hover:border-primary/40'"
            :aria-pressed="filter === c.key"
            @click="emit('filter', c.key)"
          >
            <span class="h-2 w-2 shrink-0 rounded-full" :class="c.dot" aria-hidden="true" />
            {{ t(`dashboard.stat.sessions.chip.${c.key}`, { n: c.n }) }}
          </button>
        </div>
      </div>
    </div>

    <div class="mt-auto flex min-w-0 flex-col gap-1.5">
      <div class="flex min-w-0 items-baseline justify-between gap-2">
        <span class="min-w-0 truncate text-[11px] font-semibold text-muted-foreground">{{ t('dashboard.stat.sessions.activity') }}</span>
        <span class="shrink-0 font-mono text-xs font-semibold text-primary tabular-nums">{{ t('dashboard.stat.sessions.rate', { n: rate }) }}</span>
      </div>
      <AreaChart :values="activity" :height="110" :floor="5" tone="primary" dot :label="t('dashboard.stat.sessions.activityLabel')" />
      <div v-if="sessions.length" class="mt-1 flex flex-wrap gap-1" role="group" :aria-label="t('dashboard.stat.sessions.mapLabel')">
        <button
          v-for="s in sessions"
          :key="s.id"
          type="button"
          class="h-3.5 w-3.5 cursor-pointer rounded-[4px] transition-transform hover:scale-125 motion-reduce:transition-none focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-primary"
          :class="[STATE_FILL[s.state], selectedId === s.id && 'ring-2 ring-primary ring-offset-1 ring-offset-card']"
          :title="tip(s)"
          :aria-label="tip(s)"
          @click="emit('pick', s.id)"
        />
      </div>
    </div>
  </BentoCard>
</template>
