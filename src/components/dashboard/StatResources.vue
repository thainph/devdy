<script setup lang="ts">
// Stat card "Resources": CPU (with a short history) and RAM of the processes
// Devdy manages, how many there are, and the leftovers — with "Clean up",
// which reuses the Background processes cleanup (confirmed by the caller).
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Cpu } from 'lucide-vue-next'
import { Button, Card } from '@/components/ui'
import { formatCpu, formatRss } from '@/lib/backgroundProcesses'
import type { ResourcesView } from '@/composables/useRuntimeDashboard'
import SparkLine from './SparkLine.vue'

const props = defineProps<{ resources: ResourcesView | null; busy: boolean }>()
const emit = defineEmits<{ clean: [] }>()
const { t } = useI18n()

const r = computed(() => props.resources)
const cpuTone = computed(() => ((r.value?.cpuShare ?? 0) >= 70 ? 'warning' : 'primary'))
const ramPct = computed(() => (r.value && r.value.totalMemKb > 0 ? Math.min(100, (r.value.rssKb / r.value.totalMemKb) * 100) : null))
</script>

<template>
  <Card body-class="flex h-full flex-col gap-2.5 p-4" :aria-label="t('dashboard.stat.resources.title')">
    <div class="flex items-center gap-2 text-xs font-semibold text-muted-foreground">
      <span class="grid h-6 w-6 place-items-center rounded-md bg-success/15 text-success">
        <Cpu class="h-3.5 w-3.5" :stroke-width="2" aria-hidden="true" />
      </span>
      {{ t('dashboard.stat.resources.title') }}
      <span v-if="r" class="ml-auto font-medium">{{ t('dashboard.stat.resources.processes', { n: r.processCount }, r.processCount) }}</span>
    </div>
    <p v-if="!r" class="text-[11px] text-muted-foreground">{{ t('dashboard.stat.resources.noData') }}</p>
    <template v-else>
      <div class="grid grid-cols-2 gap-3.5">
        <div class="flex min-w-0 flex-col gap-1" :title="t('dashboard.stat.resources.cpuTitle')">
          <span class="text-[11px] font-semibold text-muted-foreground">{{ t('dashboard.stat.resources.cpu') }}</span>
          <div class="flex items-end gap-2">
            <span class="text-xl leading-tight font-bold tabular-nums" :class="cpuTone === 'warning' ? 'text-warning' : 'text-foreground'">{{ formatCpu(r.cpuShare) }}</span>
            <SparkLine
              v-if="r.cpuHistory.length > 1"
              :values="r.cpuHistory"
              :width="60"
              :height="20"
              class="ml-auto"
              :tone="cpuTone === 'warning' ? 'text-warning' : 'text-primary'"
            />
          </div>
          <div class="h-1.5 overflow-hidden rounded-full bg-muted">
            <div class="h-full rounded-full" :class="cpuTone === 'warning' ? 'bg-warning' : 'bg-primary'" :style="{ width: `${r.cpuShare}%` }" />
          </div>
        </div>
        <div class="flex min-w-0 flex-col gap-1">
          <span class="text-[11px] font-semibold text-muted-foreground">{{ t('dashboard.stat.resources.ram') }}</span>
          <div class="flex items-baseline gap-1">
            <span class="text-xl leading-tight font-bold tabular-nums">{{ formatRss(r.rssKb) }}</span>
            <span v-if="r.totalMemKb > 0" class="truncate text-xs text-muted-foreground">{{ t('dashboard.stat.resources.ofTotal', { total: formatRss(r.totalMemKb) }) }}</span>
          </div>
          <div class="h-1.5 overflow-hidden rounded-full bg-muted">
            <div class="h-full rounded-full bg-primary" :style="{ width: `${ramPct ?? 0}%` }" />
          </div>
        </div>
      </div>
      <div class="mt-auto flex flex-wrap items-center gap-1.5 border-t border-dashed border-border pt-2">
        <span v-if="r.orphanCount" class="rounded-full bg-warning/15 px-2 py-0.5 text-[10px] font-semibold text-warning">{{ t('dashboard.stat.resources.orphans', { n: r.orphanCount }, r.orphanCount) }}</span>
        <span v-if="r.staleWorktreeCount" class="rounded-full bg-muted px-2 py-0.5 text-[10px] font-semibold text-muted-foreground">{{ t('dashboard.stat.resources.staleWorktrees', { n: r.staleWorktreeCount }, r.staleWorktreeCount) }}</span>
        <Button
          variant="outline"
          size="xs"
          class="ml-auto"
          :disabled="busy || !r.orphanCount"
          :title="t('dashboard.stat.resources.cleanTitle')"
          @click="emit('clean')"
        >
          {{ t('dashboard.stat.resources.clean') }}
        </Button>
      </div>
    </template>
  </Card>
</template>
