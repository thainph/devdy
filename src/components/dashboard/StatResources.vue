<script setup lang="ts">
// Bento tile "Resources", compact: CPU and RAM of the processes Devdy manages
// as small live area charts with their current values, how many processes
// there are and — only when there are any — the leftovers with "Clean up",
// which reuses the Background processes cleanup (confirmed by the caller).
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Cpu, TriangleAlert } from 'lucide-vue-next'
import { Button } from '@/components/ui'
import { formatCpu, formatRss } from '@/lib/backgroundProcesses'
import type { ResourcesView } from '@/composables/useRuntimeDashboard'
import AreaChart from './AreaChart.vue'
import BentoCard from './BentoCard.vue'

const props = defineProps<{ resources: ResourcesView | null; busy: boolean }>()
const emit = defineEmits<{ clean: [] }>()
const { t } = useI18n()

const r = computed(() => props.resources)
const cpuHot = computed(() => (r.value?.cpuShare ?? 0) >= 70)
/** RAM keeps a little headroom above its highest sample, so small swings stay small. */
const ramFloor = computed(() => Math.max(1, ...(r.value?.ramHistory ?? [])) * 1.25)
</script>

<template>
  <BentoCard :title="t('dashboard.stat.resources.title')" :icon="Cpu" tone="success" compact>
    <template #aside>
      <span v-if="r" class="ml-auto shrink-0 font-medium whitespace-nowrap">{{ t('dashboard.stat.resources.processes', { n: r.processCount }, r.processCount) }}</span>
    </template>
    <p v-if="!r" class="text-[11px] text-muted-foreground">{{ t('dashboard.stat.resources.noData') }}</p>
    <template v-else>
      <div class="grid min-h-0 grid-cols-2 gap-2.5">
        <div class="flex min-w-0 flex-col gap-0.5" :title="t('dashboard.stat.resources.cpuTitle')">
          <div class="flex min-w-0 items-baseline justify-between gap-1.5">
            <span class="truncate text-[10px] font-semibold text-muted-foreground">{{ t('dashboard.stat.resources.cpu') }}</span>
            <span class="shrink-0 text-[15px] leading-tight font-extrabold whitespace-nowrap tabular-nums" :class="cpuHot && 'text-warning'">{{ formatCpu(r.cpuShare) }}</span>
          </div>
          <AreaChart
            :values="r.cpuShareHistory"
            :max="100"
            :height="30"
            :tone="cpuHot ? 'warning' : 'success'"
            :label="t('dashboard.stat.resources.history', { name: t('dashboard.stat.resources.cpu') })"
          />
        </div>
        <div class="flex min-w-0 flex-col gap-0.5" :title="r.totalMemKb > 0 ? t('dashboard.stat.resources.ofTotal', { total: formatRss(r.totalMemKb) }) : undefined">
          <div class="flex min-w-0 items-baseline justify-between gap-1.5">
            <span class="truncate text-[10px] font-semibold text-muted-foreground">{{ t('dashboard.stat.resources.ram') }}</span>
            <span class="shrink-0 text-[15px] leading-tight font-extrabold whitespace-nowrap tabular-nums">{{ formatRss(r.rssKb) }}</span>
          </div>
          <AreaChart
            :values="r.ramHistory"
            :floor="ramFloor"
            :height="30"
            tone="primary"
            :label="t('dashboard.stat.resources.history', { name: t('dashboard.stat.resources.ram') })"
          />
        </div>
      </div>
      <div v-if="r.orphanCount" class="mt-auto flex min-w-0 items-center gap-1.5 rounded-md bg-warning/10 px-2 py-0.5">
        <TriangleAlert class="h-3 w-3 shrink-0 text-warning" :stroke-width="2" aria-hidden="true" />
        <span
          class="min-w-0 flex-1 truncate text-[11px] font-semibold"
          :title="r.staleWorktreeCount ? t('dashboard.stat.resources.staleWorktrees', { n: r.staleWorktreeCount }, r.staleWorktreeCount) : undefined"
        >{{ t('dashboard.stat.resources.orphans', { n: r.orphanCount }, r.orphanCount) }}</span>
        <Button variant="outline" size="xs" class="h-5 px-1.5 text-[10px]" :disabled="busy" :title="t('dashboard.stat.resources.cleanTitle')" @click="emit('clean')">
          {{ t('dashboard.stat.resources.clean') }}
        </Button>
      </div>
      <p v-else-if="r.staleWorktreeCount" class="mt-auto truncate text-[11px] text-muted-foreground">
        {{ t('dashboard.stat.resources.staleWorktrees', { n: r.staleWorktreeCount }, r.staleWorktreeCount) }}
      </p>
    </template>
  </BentoCard>
</template>
