<script setup lang="ts">
// Stat card "5-hour limit": Claude + Codex — % used, a gauge, when it runs out
// at the current pace (when that can be told) and when it resets.
import { useI18n } from 'vue-i18n'
import { Clock } from 'lucide-vue-next'
import { Card } from '@/components/ui'
import type { QuotaView } from '@/composables/useRuntimeDashboard'
import EngineMark from './EngineMark.vue'

defineProps<{ quotas: QuotaView[] }>()
const { t } = useI18n()

function hm(ms: number): string {
  return new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
}
function tone(pct: number | null): { text: string; bar: string } {
  if (pct === null) return { text: 'text-muted-foreground', bar: 'bg-muted-foreground/30' }
  if (pct >= 90) return { text: 'text-destructive', bar: 'bg-destructive' }
  if (pct >= 70) return { text: 'text-warning', bar: 'bg-warning' }
  return { text: 'text-foreground', bar: 'bg-primary' }
}
function note(q: QuotaView): string {
  if (q.pct === null) return t('dashboard.stat.quota.noData')
  const f = q.forecast
  const head =
    f.kind === 'exhaust' ? t('dashboard.stat.quota.exhaust', { time: hm(f.at) })
    : f.kind === 'full' ? t('dashboard.stat.quota.full')
    : f.kind === 'safe' ? t('dashboard.stat.quota.safe')
    : t('dashboard.stat.quota.unknown')
  return q.resetsAt !== null ? `${head} · ${t('dashboard.stat.quota.reset', { time: hm(q.resetsAt) })}` : head
}
</script>

<template>
  <Card body-class="flex h-full flex-col gap-2.5 p-4" :aria-label="t('dashboard.stat.quota.title')">
    <div class="flex items-center gap-2 text-xs font-semibold text-muted-foreground">
      <span class="grid h-6 w-6 place-items-center rounded-md bg-warning/15 text-warning">
        <Clock class="h-3.5 w-3.5" :stroke-width="2" aria-hidden="true" />
      </span>
      {{ t('dashboard.stat.quota.title') }}
    </div>
    <div v-for="q in quotas" :key="q.engine" class="flex min-w-0 flex-col gap-1">
      <div class="flex items-baseline gap-2">
        <EngineMark :engine="q.engine" size="sm" />
        <span class="flex-1 text-[13px] font-semibold">{{ t(`dashboard.engine.${q.engine}`) }}</span>
        <span class="text-xl leading-tight font-bold tabular-nums" :class="tone(q.pct).text">{{ q.pct !== null ? `${Math.round(q.pct)}%` : '–' }}</span>
      </div>
      <div class="h-1.5 overflow-hidden rounded-full bg-muted">
        <div class="h-full rounded-full" :class="tone(q.pct).bar" :style="{ width: `${Math.min(100, q.pct ?? 0)}%` }" />
      </div>
      <div class="truncate text-[11px] text-muted-foreground" :title="note(q)">{{ note(q) }}</div>
    </div>
  </Card>
</template>
