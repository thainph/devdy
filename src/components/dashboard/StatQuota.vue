<script setup lang="ts">
// Bento tile "5-hour limit": one ring per Claude account (or one for Claude)
// plus Codex — % used in the centre, coloured by level, the account name, a
// "default" badge and a live countdown to the reset. The forecast (when it runs
// out at the current pace) is in the tooltip.
import { useI18n } from 'vue-i18n'
import { Clock } from 'lucide-vue-next'
import type { QuotaView } from '@/composables/useRuntimeDashboard'
import { formatCountdown, quotaLevel, ringDash, type QuotaLevel } from '@/lib/runtimeCharts'
import BentoCard from './BentoCard.vue'
import EngineMark from './EngineMark.vue'

defineProps<{ quotas: QuotaView[]; now: number }>()
const { t } = useI18n()

const R = 29
const LEVEL: Record<QuotaLevel, { text: string; stroke: string }> = {
  none: { text: 'text-muted-foreground', stroke: 'stroke-muted-foreground/40' },
  ok: { text: 'text-info', stroke: 'stroke-info' },
  warning: { text: 'text-warning', stroke: 'stroke-warning' },
  danger: { text: 'text-destructive', stroke: 'stroke-destructive' },
}

function name(q: QuotaView): string {
  return q.label ?? t(`dashboard.engine.${q.engine}`)
}
function hm(ms: number): string {
  return new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
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
function countdown(q: QuotaView, now: number): string {
  return q.resetsAt !== null ? formatCountdown(q.resetsAt - now) : ''
}
</script>

<template>
  <BentoCard :title="t('dashboard.stat.quota.title')" :icon="Clock" tone="warning">
    <ul class="grid flex-1 grid-cols-[repeat(auto-fit,minmax(76px,1fr))] content-center gap-x-2 gap-y-3">
      <li
        v-for="q in quotas"
        :key="q.key"
        class="flex min-w-0 flex-col items-center gap-1"
        :title="`${name(q)} · ${q.pct !== null ? `${Math.round(q.pct)}% · ` : ''}${note(q)}`"
      >
        <div class="relative h-[70px] w-[70px]">
          <svg viewBox="0 0 70 70" class="h-full w-full" aria-hidden="true">
            <circle cx="35" cy="35" :r="R" fill="none" class="stroke-muted" stroke-width="7" />
            <circle
              v-if="q.pct"
              cx="35"
              cy="35"
              :r="R"
              fill="none"
              stroke-width="7"
              stroke-linecap="round"
              :stroke-dasharray="ringDash(q.pct, R)"
              transform="rotate(-90 35 35)"
              class="rt-grow"
              :class="LEVEL[quotaLevel(q.pct)].stroke"
            />
          </svg>
          <span class="absolute inset-0 grid place-items-center text-[15px] font-extrabold tabular-nums" :class="LEVEL[quotaLevel(q.pct)].text">
            {{ q.pct !== null ? `${Math.round(q.pct)}%` : '–' }}
          </span>
        </div>
        <span class="flex max-w-full min-w-0 items-center gap-1">
          <EngineMark :engine="q.engine" size="sm" />
          <span class="min-w-0 truncate text-[11px] font-bold">{{ name(q) }}</span>
        </span>
        <span class="font-mono text-[10px] whitespace-nowrap text-muted-foreground tabular-nums">
          <template v-if="countdown(q, now)">
            <span aria-hidden="true">↻ {{ countdown(q, now) }}</span>
            <span class="sr-only">{{ t('dashboard.stat.quota.resetIn', { time: countdown(q, now) }) }}</span>
          </template>
          <template v-else>{{ t('dashboard.stat.quota.noData') }}</template>
        </span>
        <span v-if="q.isDefault" class="rounded-full bg-muted px-1.5 text-[9px] font-semibold text-muted-foreground">{{ t('dashboard.stat.quota.default') }}</span>
      </li>
    </ul>
  </BentoCard>
</template>
