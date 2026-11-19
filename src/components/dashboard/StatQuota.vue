<script setup lang="ts">
// Bento tile "5-hour limit", compact. Up to QUOTA_RING_MAX limits: one ring
// each (Claude per account, plus Codex) — % used in the centre, coloured by
// level, the account name and a live countdown to the reset. More than that
// (several Claude accounts): one row each — name, bar, %, reset — highest use
// first, Codex last, scrolling inside the tile. The forecast (when it runs out
// at the current pace) is in the tooltip.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Clock } from 'lucide-vue-next'
import type { QuotaView } from '@/composables/useRuntimeDashboard'
import { QUOTA_RING_MAX, formatCountdown, quotaLevel, quotaListOrder, ringDash, type QuotaLevel } from '@/lib/runtimeCharts'
import BentoCard from './BentoCard.vue'
import EngineMark from './EngineMark.vue'

const props = defineProps<{ quotas: QuotaView[]; now: number }>()
const { t } = useI18n()

const R = 29
const LEVEL: Record<QuotaLevel, { text: string; stroke: string; bar: string }> = {
  none: { text: 'text-muted-foreground', stroke: 'stroke-muted-foreground/40', bar: 'bg-muted-foreground/40' },
  ok: { text: 'text-info', stroke: 'stroke-info', bar: 'bg-info' },
  warning: { text: 'text-warning', stroke: 'stroke-warning', bar: 'bg-warning' },
  danger: { text: 'text-destructive', stroke: 'stroke-destructive', bar: 'bg-destructive' },
}

const asList = computed(() => props.quotas.length > QUOTA_RING_MAX)
const listed = computed(() => quotaListOrder(props.quotas))

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
function tip(q: QuotaView): string {
  return `${name(q)} · ${q.pct !== null ? `${Math.round(q.pct)}% · ` : ''}${note(q)}`
}
function countdown(q: QuotaView, now: number): string {
  return q.resetsAt !== null ? formatCountdown(q.resetsAt - now) : ''
}
function pctText(q: QuotaView): string {
  return q.pct !== null ? `${Math.round(q.pct)}%` : '–'
}
</script>

<template>
  <BentoCard :title="t('dashboard.stat.quota.title')" :icon="Clock" tone="warning" compact>
    <!-- Several accounts: one row each -->
    <ul v-if="asList" class="flex min-h-0 flex-1 flex-col gap-1 overflow-y-auto pr-0.5">
      <li
        v-for="q in listed"
        :key="q.key"
        class="grid min-w-0 grid-cols-[14px_minmax(0,1fr)_minmax(36px,56px)_30px_44px] items-center gap-1.5 text-[11px]"
        :title="tip(q)"
      >
        <EngineMark :engine="q.engine" size="sm" />
        <span class="flex min-w-0 items-center gap-1">
          <span class="min-w-0 truncate">{{ name(q) }}</span>
          <span v-if="q.isDefault" class="shrink-0 rounded-full bg-muted px-1.5 text-[9px] font-semibold text-muted-foreground">{{ t('dashboard.stat.quota.default') }}</span>
        </span>
        <span class="relative h-[5px] overflow-hidden rounded-full bg-muted" aria-hidden="true">
          <span class="absolute inset-y-0 left-0 rounded-full" :class="LEVEL[quotaLevel(q.pct)].bar" :style="{ width: `${q.pct ?? 0}%` }" />
        </span>
        <span class="text-right font-bold tabular-nums" :class="LEVEL[quotaLevel(q.pct)].text">{{ pctText(q) }}</span>
        <span class="truncate text-right font-mono text-[10px] text-muted-foreground tabular-nums">
          <template v-if="countdown(q, now)">
            <span aria-hidden="true">↻ {{ countdown(q, now) }}</span>
            <span class="sr-only">{{ t('dashboard.stat.quota.resetIn', { time: countdown(q, now) }) }}</span>
          </template>
          <template v-else>–</template>
        </span>
      </li>
    </ul>

    <!-- A few: rings -->
    <ul v-else class="flex min-h-0 flex-1 items-center justify-around gap-2">
      <li v-for="q in quotas" :key="q.key" class="flex min-w-0 flex-col items-center gap-0.5" :title="tip(q)">
        <div class="relative h-[46px] w-[46px]">
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
          <span class="absolute inset-0 grid place-items-center text-[11px] font-extrabold tabular-nums" :class="LEVEL[quotaLevel(q.pct)].text">{{ pctText(q) }}</span>
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
      </li>
    </ul>
  </BentoCard>
</template>
