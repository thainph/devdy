<script setup lang="ts">
// Stat card "Sessions": the total, a square per session coloured by state
// (click = select it, which also opens its group) and the quick filter chips.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Activity } from 'lucide-vue-next'
import { Card } from '@/components/ui'
import type { QuickFilter, RtSession } from '@/lib/runtimeDashboard'
import { STATE_FILL, useRuntimeLabels } from './runtimeStyles'

const props = defineProps<{
  /** Every session, in group order. */
  sessions: RtSession[]
  counts: Record<QuickFilter, number>
  filter: QuickFilter | null
  selectedId: string | null
}>()
const emit = defineEmits<{ pick: [id: string]; filter: [f: QuickFilter] }>()

const { t } = useI18n()
const L = useRuntimeLabels()

const conductors = computed(() => props.sessions.filter((s) => s.role === 'conductor').length)
const external = computed(() => props.sessions.filter((s) => s.role === 'external').length)
const chips = computed(() =>
  (['wait', 'quiet', 'ctx'] as const)
    .filter((k) => k !== 'wait' || props.counts.wait > 0)
    .map((k) => ({ key: k, n: props.counts[k], dot: k === 'quiet' ? STATE_FILL.quiet : 'bg-warning' })),
)
function tip(s: RtSession): string {
  return [s.title, s.projectName, L.stateLabel(s.state)].filter(Boolean).join(' · ')
}
</script>

<template>
  <Card body-class="flex h-full flex-col gap-2.5 p-4" :aria-label="t('dashboard.stat.sessions.title')">
    <div class="flex items-center gap-2 text-xs font-semibold text-muted-foreground">
      <span class="grid h-6 w-6 place-items-center rounded-md bg-primary/15 text-primary">
        <Activity class="h-3.5 w-3.5" :stroke-width="2" aria-hidden="true" />
      </span>
      <span class="min-w-0 truncate">{{ t('dashboard.stat.sessions.title') }}</span>
    </div>
    <div class="flex min-w-0 items-baseline gap-2">
      <span class="shrink-0 text-3xl leading-none font-bold whitespace-nowrap tabular-nums">{{ sessions.length }}</span>
      <span class="shrink-0 text-[13px] font-semibold whitespace-nowrap">{{ t('dashboard.stat.sessions.unit', sessions.length) }}</span>
      <span class="min-w-0 truncate text-xs text-muted-foreground" :title="t('dashboard.stat.sessions.meta', { conductors, external }, conductors)">· {{ t('dashboard.stat.sessions.meta', { conductors, external }, conductors) }}</span>
    </div>
    <div v-if="sessions.length" class="flex flex-wrap gap-1" role="group" :aria-label="t('dashboard.stat.sessions.mapLabel')">
      <button
        v-for="s in sessions"
        :key="s.id"
        type="button"
        class="h-[13px] w-[13px] cursor-pointer rounded-[4px] transition-transform hover:scale-125 motion-reduce:transition-none focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-primary"
        :class="[STATE_FILL[s.state], selectedId === s.id && 'ring-2 ring-primary ring-offset-1 ring-offset-card']"
        :title="tip(s)"
        :aria-label="tip(s)"
        @click="emit('pick', s.id)"
      />
    </div>
    <div class="mt-auto flex flex-wrap gap-1.5">
      <button
        v-for="c in chips"
        :key="c.key"
        type="button"
        class="inline-flex min-h-7 cursor-pointer items-center gap-1.5 rounded-full border px-2.5 text-xs font-semibold transition-colors"
        :class="filter === c.key ? 'border-primary bg-primary/15 text-primary' : 'border-border bg-card text-foreground hover:border-primary/40'"
        :aria-pressed="filter === c.key"
        @click="emit('filter', c.key)"
      >
        <span class="h-2 w-2 shrink-0 rounded-full" :class="c.dot" aria-hidden="true" />
        {{ t(`dashboard.stat.sessions.chip.${c.key}`, { n: c.n }) }}
      </button>
    </div>
  </Card>
</template>
