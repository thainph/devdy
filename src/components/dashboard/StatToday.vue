<script setup lang="ts">
// Bento tile "Today", compact: runs finished today and how many failed, the
// busiest hour, and a timeline from 08:00 (or the day start, when a run
// finished earlier) to now with one dot per run finished today. The latest
// runs themselves are in the Dashboard's History.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { CircleCheckBig } from 'lucide-vue-next'
import type { RuntimeToday, TimelineRun } from '@/lib/runtimeSnapshot'
import {
  busiestHour,
  clockLabel,
  timelineDots,
  timelineTicks,
  timelineWindow,
} from '@/lib/runtimeCharts'
import BentoCard from './BentoCard.vue'
import CountUp from './CountUp.vue'

const props = defineProps<{ today: RuntimeToday | null; now: number }>()
const { t } = useI18n()

const runs = computed<TimelineRun[]>(() => props.today?.timeline ?? [])
const firstAt = computed(() => {
  const first = runs.value.length ? Date.parse(runs.value[0].finishedAt) : NaN
  return Number.isNaN(first) ? null : first
})
// The window moves with the clock; the dots and marks follow it once a second.
const span = computed(() => timelineWindow(firstAt.value, props.now))
const dots = computed(() => timelineDots(runs.value, span.value))
const ticks = computed(() => timelineTicks(span.value))
const busiest = computed(() => busiestHour(dots.value.map((d) => d.at)))

const DOT: Record<TimelineRun['status'], string> = {
  done: 'bg-success',
  failed: 'bg-destructive',
  cancelled: 'bg-muted-foreground',
}
function dotTitle(r: TimelineRun, at: number): string {
  return `${r.title || t('dashboard.untitled')} · ${clockLabel(at)} · ${t(`dashboard.stat.today.status.${r.status}`)}`
}
</script>

<template>
  <BentoCard :title="t('dashboard.stat.today.title')" :icon="CircleCheckBig" tone="muted" compact>
    <template #aside>
      <span v-if="busiest" class="ml-auto shrink-0 truncate font-mono text-[10px] font-medium">
        {{ t('dashboard.stat.today.busiest', { from: clockLabel(busiest.start), to: clockLabel(busiest.start + 3_600_000) }) }}
      </span>
    </template>
    <p v-if="!today" class="text-[11px] text-muted-foreground">{{ t('dashboard.stat.today.noData') }}</p>
    <template v-else>
      <div class="flex min-w-0 items-baseline gap-x-2">
        <CountUp :value="today.finished" class="text-[26px] leading-none font-extrabold tracking-tight" />
        <span class="text-xs font-bold whitespace-nowrap">{{ t('dashboard.stat.today.finished') }}</span>
        <span v-if="today.failed" class="shrink-0 rounded-full bg-destructive/15 px-2 py-px text-[10px] font-semibold whitespace-nowrap text-destructive">{{ t('dashboard.stat.today.failed', { n: today.failed }) }}</span>
      </div>
      <p v-if="!dots.length && !today.finished && !today.failed" class="text-[11px] text-muted-foreground">{{ t('dashboard.stat.today.empty') }}</p>
      <div class="mt-auto flex flex-col gap-0.5">
        <div
          class="relative mx-1.5 h-[18px]"
          role="img"
          :aria-label="`${t('dashboard.stat.today.timelineLabel', { from: clockLabel(span.start) })}: ${dots.length}`"
        >
          <span class="absolute inset-x-0 top-1/2 h-1 -translate-y-1/2 rounded-full bg-linear-to-r from-muted to-primary/30" aria-hidden="true" />
          <span
            v-for="d in dots"
            :key="d.run.runId"
            class="absolute top-1/2 h-2.5 w-2.5 -translate-x-1/2 -translate-y-1/2 rounded-full border-2 border-card transition-transform hover:scale-150 motion-reduce:transition-none"
            :class="DOT[d.run.status]"
            :style="{ left: `${d.pct}%` }"
            :title="dotTitle(d.run, d.at)"
          />
          <span class="absolute top-1/2 -right-0.5 h-4 w-[3px] -translate-y-1/2 rounded-full bg-primary" :title="t('dashboard.stat.today.now')" aria-hidden="true" />
        </div>
        <div class="relative mx-1.5 h-3.5 font-mono text-[10px] text-muted-foreground tabular-nums" aria-hidden="true">
          <span class="absolute left-0">{{ clockLabel(span.start) }}</span>
          <span v-for="tk in ticks" :key="tk.at" class="absolute -translate-x-1/2" :style="{ left: `${tk.pct}%` }">{{ tk.label }}</span>
          <span class="absolute right-0">{{ clockLabel(span.end) }}</span>
        </div>
      </div>
    </template>
  </BentoCard>
</template>
