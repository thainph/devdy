<script setup lang="ts">
// Bento tile "Today": runs finished today and how many failed, a timeline from
// 08:00 (or the day start, when a run finished earlier) to now with one dot per
// run finished today, and the latest finished runs (not limited to today;
// click opens the run). A run finished since the user last opened it is marked
// unread.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { CheckCircle2, CircleCheckBig, XCircle } from 'lucide-vue-next'
import type { RecentRun, RuntimeToday, TimelineRun } from '@/lib/runtimeSnapshot'
import {
  busiestHour,
  clockLabel,
  timelineDots,
  timelineTicks,
  timelineWindow,
} from '@/lib/runtimeCharts'
import BentoCard from './BentoCard.vue'
import CountUp from './CountUp.vue'

const props = defineProps<{ today: RuntimeToday | null; now: number; unread: ReadonlySet<string> }>()
const emit = defineEmits<{ open: [run: RecentRun] }>()
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
const unreadCount = computed(() => props.today?.recent.filter((r) => props.unread.has(r.runId)).length ?? 0)

const DOT: Record<TimelineRun['status'], string> = {
  done: 'bg-success',
  failed: 'bg-destructive',
  cancelled: 'bg-muted-foreground',
}
function dotTitle(r: TimelineRun, at: number): string {
  return `${r.title || t('dashboard.untitled')} · ${clockLabel(at)} · ${t(`dashboard.stat.today.status.${r.status}`)}`
}

/** Time of day; the date too when it is not today (the list is not limited to today). */
function when(iso: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return ''
  const time = d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
  if (d.toDateString() === new Date(props.now).toDateString()) return time
  return `${d.toLocaleDateString([], { day: '2-digit', month: '2-digit' })} ${time}`
}
</script>

<template>
  <BentoCard :title="t('dashboard.stat.today.title')" :icon="CircleCheckBig" tone="muted">
    <p v-if="!today" class="text-[11px] text-muted-foreground">{{ t('dashboard.stat.today.noData') }}</p>
    <div v-else class="flex flex-wrap gap-x-6 gap-y-3">
      <div class="flex min-w-0 flex-[2_1_320px] flex-col gap-2.5">
        <div class="flex min-w-0 flex-wrap items-baseline gap-x-2.5 gap-y-1">
          <CountUp :value="today.finished" class="text-4xl leading-none font-extrabold tracking-tight" />
          <span class="text-sm font-bold whitespace-nowrap">{{ t('dashboard.stat.today.finished') }}</span>
          <span v-if="today.failed" class="shrink-0 rounded-full bg-destructive/15 px-2 py-0.5 text-[10px] font-semibold whitespace-nowrap text-destructive">{{ t('dashboard.stat.today.failed', { n: today.failed }) }}</span>
          <span v-if="busiest" class="ml-auto text-[11px] whitespace-nowrap text-muted-foreground">
            {{ t('dashboard.stat.today.busiest', { from: clockLabel(busiest.start), to: clockLabel(busiest.start + 3_600_000) }) }}
          </span>
        </div>

        <div
          class="relative mx-1.5 h-[34px]"
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
          <span class="absolute top-1/2 -right-0.5 h-[22px] w-[3px] -translate-y-1/2 rounded-full bg-primary" :title="t('dashboard.stat.today.now')" aria-hidden="true" />
        </div>
        <div class="relative mx-1.5 h-3.5 font-mono text-[10px] text-muted-foreground tabular-nums" aria-hidden="true">
          <span class="absolute left-0">{{ clockLabel(span.start) }}</span>
          <span v-for="tk in ticks" :key="tk.at" class="absolute -translate-x-1/2" :style="{ left: `${tk.pct}%` }">{{ tk.label }}</span>
          <span class="absolute right-0">{{ clockLabel(span.end) }}</span>
        </div>
        <p v-if="!dots.length && !today.finished && !today.failed" class="text-[11px] text-muted-foreground">{{ t('dashboard.stat.today.empty') }}</p>
      </div>

      <div v-if="today.recent.length" class="flex min-w-0 flex-[1_1_220px] flex-col gap-1">
        <span class="flex items-center gap-1.5 text-[11px] font-semibold text-muted-foreground">
          {{ t('dashboard.stat.today.latest') }}
          <span v-if="unreadCount" class="rounded-full bg-primary/15 px-1.5 py-px text-[10px] text-primary">{{ t('dashboard.stat.today.unreadCount', { n: unreadCount }) }}</span>
        </span>
        <ul class="flex flex-col gap-0.5">
          <li v-for="r in today.recent" :key="r.runId">
            <button
              type="button"
              class="flex w-full min-w-0 cursor-pointer items-center gap-2 rounded px-1 py-0.5 text-left text-xs hover:bg-accent/50"
              @click="emit('open', r)"
            >
              <XCircle v-if="r.status === 'failed'" class="h-3.5 w-3.5 shrink-0 text-destructive" :stroke-width="1.75" aria-hidden="true" />
              <CheckCircle2 v-else class="h-3.5 w-3.5 shrink-0" :class="r.status === 'done' ? 'text-success' : 'text-muted-foreground'" :stroke-width="1.75" aria-hidden="true" />
              <span class="sr-only">{{ t(`dashboard.stat.today.status.${r.status}`) }}</span>
              <span class="min-w-0 flex-1 truncate" :class="unread.has(r.runId) && 'font-semibold'">
                {{ r.title || t('dashboard.untitled') }}
                <span class="text-muted-foreground">· {{ r.status === 'failed' && r.error ? r.error : r.projectName }}</span>
              </span>
              <span v-if="unread.has(r.runId)" class="h-1.5 w-1.5 shrink-0 rounded-full bg-primary" :title="t('dashboard.stat.today.unread')" />
              <span v-if="unread.has(r.runId)" class="sr-only">{{ t('dashboard.stat.today.unread') }}</span>
              <span class="shrink-0 font-mono text-[10px] text-muted-foreground">{{ when(r.finishedAt) }}</span>
            </button>
          </li>
        </ul>
      </div>
    </div>
  </BentoCard>
</template>
