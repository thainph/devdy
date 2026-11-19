<script setup lang="ts">
// The Processes screen's summary strip: CPU and RAM with their live charts (the
// same readings the Dashboard's Resources tile shows), the process count split
// by group, and the leftovers with a shortcut to that filter.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Boxes, Cpu, MemoryStick, TriangleAlert } from 'lucide-vue-next'
import { Button } from '@/components/ui'
import AreaChart from '@/components/dashboard/AreaChart.vue'
import BentoCard from '@/components/dashboard/BentoCard.vue'
import { formatCpu, formatRss, type ProcFilter } from '@/lib/backgroundProcesses'

const props = defineProps<{
  /** Percent of one core, summed over every tracked process. */
  cpu: number
  cpuHistory: readonly number[]
  rssKb: number
  rssHistory: readonly number[]
  /** Machine RAM in KB, 0 = unknown. */
  totalMemKb: number
  counts: Record<ProcFilter, number>
  groupCount: number
  leftoverRssKb: number
}>()
const emit = defineEmits<{ review: [] }>()
const { t } = useI18n()

const cores = typeof navigator !== 'undefined' && navigator.hardwareConcurrency > 0 ? navigator.hardwareConcurrency : 1
const share = (v: number) => Math.min(100, v / cores)
const cpuShare = computed(() => share(props.cpu))
const cpuShareHistory = computed(() => props.cpuHistory.map(share))
const cpuHot = computed(() => cpuShare.value >= 70)
/** RAM keeps a little headroom above its highest sample, so small swings stay small. */
const ramFloor = computed(() => Math.max(1, ...props.rssHistory) * 1.25)
const ramPct = computed(() => (props.totalMemKb > 0 ? `${((props.rssKb / props.totalMemKb) * 100).toFixed(1)}%` : null))

const SEGMENTS = [
  { key: 'leftovers', dot: 'bg-warning' },
  { key: 'sessions', dot: 'bg-primary' },
  { key: 'services', dot: 'bg-muted-foreground' },
] as const
</script>

<template>
  <div class="grid grid-cols-2 gap-3 xl:grid-cols-[1.3fr_1.3fr_1fr_1fr]">
    <BentoCard :title="t('processes.col.cpu')" :icon="Cpu" :tone="cpuHot ? 'warning' : 'success'">
      <template #aside>
        <span class="ml-auto shrink-0 text-[11px] font-medium">{{ t('processes.summary.cpuOf', { n: cores }) }}</span>
      </template>
      <div class="flex items-baseline gap-2">
        <span class="text-xl font-extrabold tabular-nums" :class="cpuHot && 'text-warning'">{{ formatCpu(cpuShare) }}</span>
        <span class="truncate text-[11px] text-muted-foreground">{{ t('processes.summary.cpuOneCore', { pct: formatCpu(cpu) }) }}</span>
      </div>
      <AreaChart
        :values="cpuShareHistory"
        :max="100"
        :height="36"
        :tone="cpuHot ? 'warning' : 'success'"
        dot
        :label="t('dashboard.stat.resources.history', { name: t('processes.col.cpu') })"
      />
    </BentoCard>

    <BentoCard :title="t('processes.col.ram')" :icon="MemoryStick" tone="primary">
      <template #aside>
        <span v-if="totalMemKb > 0" class="ml-auto shrink-0 text-[11px] font-medium">{{ t('processes.summary.ramOf', { total: formatRss(totalMemKb) }) }}</span>
      </template>
      <div class="flex items-baseline gap-2">
        <span class="text-xl font-extrabold tabular-nums">{{ formatRss(rssKb) }}</span>
        <span v-if="ramPct" class="truncate text-[11px] text-muted-foreground">{{ t('processes.summary.ramShare', { pct: ramPct }) }}</span>
      </div>
      <AreaChart
        :values="rssHistory"
        :floor="ramFloor"
        :height="36"
        tone="primary"
        dot
        :label="t('dashboard.stat.resources.history', { name: t('processes.col.ram') })"
      />
    </BentoCard>

    <BentoCard :title="t('processes.summary.processes')" :icon="Boxes" tone="muted">
      <div class="flex items-baseline gap-2">
        <span class="text-xl font-extrabold tabular-nums">{{ counts.all }}</span>
        <span class="truncate text-[11px] text-muted-foreground">{{ t('processes.summary.groups', { n: groupCount }, groupCount) }}</span>
      </div>
      <div class="flex h-1.5 gap-0.5 overflow-hidden rounded-full bg-muted" aria-hidden="true">
        <template v-for="s in SEGMENTS" :key="s.key">
          <span v-if="counts[s.key] > 0" :class="s.dot" :style="{ flex: counts[s.key] }" />
        </template>
      </div>
      <div class="flex flex-wrap gap-x-3 gap-y-0.5 text-[11px] text-muted-foreground">
        <span v-for="s in SEGMENTS" :key="s.key" class="inline-flex items-center gap-1">
          <span class="h-1.5 w-1.5 rounded-full" :class="s.dot" />
          {{ t(`processes.filter.${s.key}`) }} <span class="tabular-nums">{{ counts[s.key] }}</span>
        </span>
      </div>
    </BentoCard>

    <BentoCard :title="t('processes.summary.leftovers')" :icon="TriangleAlert" :tone="counts.leftovers ? 'warning' : 'muted'">
      <div class="flex items-baseline gap-2">
        <span class="text-xl font-extrabold tabular-nums" :class="counts.leftovers ? 'text-warning' : 'text-muted-foreground'">{{ counts.leftovers }}</span>
        <span class="truncate text-[11px] text-muted-foreground">
          {{ counts.leftovers ? t('processes.summary.held', { size: formatRss(leftoverRssKb) }) : t('processes.summary.none') }}
        </span>
      </div>
      <div v-if="counts.leftovers" class="mt-auto flex justify-end">
        <Button variant="outline" size="xs" @click="emit('review')">{{ t('processes.summary.review') }}</Button>
      </div>
    </BentoCard>
  </div>
</template>
