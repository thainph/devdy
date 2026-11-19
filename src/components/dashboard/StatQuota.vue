<script setup lang="ts">
// Bento tile "Limits", compact: the 5-hour and the weekly plan limits of each
// Claude account (or Claude) and Codex. Up to QUOTA_COLUMNS_MAX: one column
// each — the name, then two bars stacked (5 hours over week) with their %.
// More (several Claude accounts): one row each — name, 5-hour bar + %, week %
// — highest 5-hour use first, Codex last, scrolling inside the tile. Colour
// follows the level; both resets (and the 5-hour forecast) are in the tooltip.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Clock } from 'lucide-vue-next'
import type { QuotaView } from '@/composables/useRuntimeDashboard'
import { QUOTA_COLUMNS_MAX, formatCountdown, quotaLevel, quotaListOrder, type QuotaLevel } from '@/lib/runtimeCharts'
import BentoCard from './BentoCard.vue'

const props = defineProps<{ quotas: QuotaView[]; now: number }>()
const { t } = useI18n()

const LEVEL: Record<QuotaLevel, { text: string; bar: string }> = {
  none: { text: 'text-muted-foreground', bar: 'bg-muted-foreground/40' },
  ok: { text: 'text-info', bar: 'bg-info' },
  warning: { text: 'text-warning', bar: 'bg-warning' },
  danger: { text: 'text-destructive', bar: 'bg-destructive' },
}

const asList = computed(() => props.quotas.length > QUOTA_COLUMNS_MAX)
const listed = computed(() => quotaListOrder(props.quotas))

function name(q: QuotaView): string {
  return q.label ?? t(`dashboard.engine.${q.engine}`)
}
function hm(ms: number): string {
  return new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
}
function dayTime(ms: number): string {
  return new Date(ms).toLocaleString([], { weekday: 'short', hour: '2-digit', minute: '2-digit' })
}
function forecast(q: QuotaView): string {
  const f = q.forecast
  return f.kind === 'exhaust' ? t('dashboard.stat.quota.exhaust', { time: hm(f.at) })
    : f.kind === 'full' ? t('dashboard.stat.quota.full')
    : f.kind === 'safe' ? t('dashboard.stat.quota.safe')
    : t('dashboard.stat.quota.unknown')
}
function tip(q: QuotaView): string {
  const five = q.pct === null
    ? `${t('dashboard.stat.quota.fiveHour')}: ${t('dashboard.stat.quota.noData')}`
    : `${t('dashboard.stat.quota.fiveHour')} ${Math.round(q.pct)}%${q.resetsAt !== null ? ` · ${t('dashboard.stat.quota.resetIn', { time: formatCountdown(q.resetsAt - props.now) })}` : ''} · ${forecast(q)}`
  const week = q.weekPct === null
    ? `${t('dashboard.stat.quota.week')}: ${t('dashboard.stat.quota.noData')}`
    : `${t('dashboard.stat.quota.week')} ${Math.round(q.weekPct)}%${q.weekResetsAt !== null ? ` · ${t('dashboard.stat.quota.reset', { time: dayTime(q.weekResetsAt) })}` : ''}`
  return `${name(q)}\n${five}\n${week}`
}
function pctText(pct: number | null): string {
  return pct !== null ? `${Math.round(pct)}%` : '–'
}
</script>

<template>
  <BentoCard :title="t('dashboard.stat.quota.title')" :icon="Clock" tone="warning" compact>
    <!-- Several accounts: one row each -->
    <div v-if="asList" class="flex min-h-0 flex-1 flex-col gap-1 overflow-y-auto pr-0.5">
      <div class="grid grid-cols-[minmax(0,1fr)_minmax(36px,56px)_30px_34px] gap-1.5 text-[10px] font-semibold text-muted-foreground">
        <span />
        <span class="col-span-2">{{ t('dashboard.stat.quota.fiveHour') }}</span>
        <span class="text-right">{{ t('dashboard.stat.quota.week') }}</span>
      </div>
      <div
        v-for="q in listed"
        :key="q.key"
        class="grid min-w-0 grid-cols-[minmax(0,1fr)_minmax(36px,56px)_30px_34px] items-center gap-1.5 text-[11px]"
        :title="tip(q)"
      >
        <span class="flex min-w-0 items-center gap-1">
          <span class="min-w-0 truncate font-semibold">{{ name(q) }}</span>
          <span v-if="q.isDefault" class="shrink-0 rounded-full bg-muted px-1.5 text-[9px] font-semibold text-muted-foreground">{{ t('dashboard.stat.quota.default') }}</span>
        </span>
        <span class="relative h-[5px] overflow-hidden rounded-full bg-muted" aria-hidden="true">
          <span class="absolute inset-y-0 left-0 rounded-full" :class="LEVEL[quotaLevel(q.pct)].bar" :style="{ width: `${q.pct ?? 0}%` }" />
        </span>
        <span class="text-right font-bold tabular-nums" :class="LEVEL[quotaLevel(q.pct)].text">{{ pctText(q.pct) }}</span>
        <span class="text-right font-bold tabular-nums" :class="LEVEL[quotaLevel(q.weekPct)].text">{{ pctText(q.weekPct) }}</span>
      </div>
    </div>

    <!-- A few: one column each, 5 hours over week -->
    <ul v-else class="grid min-h-0 flex-1 auto-cols-fr grid-flow-col content-center gap-3">
      <li v-for="q in quotas" :key="q.key" class="flex min-w-0 flex-col gap-1.5" :title="tip(q)">
        <span class="flex min-w-0 items-center gap-1">
          <span class="min-w-0 truncate text-[11px] font-bold">{{ name(q) }}</span>
          <span v-if="q.isDefault" class="shrink-0 rounded-full bg-muted px-1.5 text-[9px] font-semibold text-muted-foreground">{{ t('dashboard.stat.quota.default') }}</span>
        </span>
        <span
          v-for="bar in [
            { label: t('dashboard.stat.quota.fiveHour'), pct: q.pct },
            { label: t('dashboard.stat.quota.week'), pct: q.weekPct },
          ]"
          :key="bar.label"
          class="flex min-w-0 flex-col gap-0.5"
        >
          <span class="flex min-w-0 items-baseline justify-between gap-1">
            <span class="truncate text-[10px] font-semibold text-muted-foreground">{{ bar.label }}</span>
            <span class="text-[11px] font-bold tabular-nums" :class="LEVEL[quotaLevel(bar.pct)].text">{{ pctText(bar.pct) }}</span>
          </span>
          <span class="relative h-[5px] overflow-hidden rounded-full bg-muted" aria-hidden="true">
            <span class="absolute inset-y-0 left-0 rounded-full" :class="LEVEL[quotaLevel(bar.pct)].bar" :style="{ width: `${bar.pct ?? 0}%` }" />
          </span>
        </span>
      </li>
    </ul>
  </BentoCard>
</template>
