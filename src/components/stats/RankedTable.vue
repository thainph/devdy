<script setup lang="ts">
// A Stats breakdown (by project / model / account): name with a share bar
// against the top row, tokens, cost and share of the total. Shows the top
// `limit` rows with a "Show all" toggle so the three cards sit level.
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Card } from '@/components/ui'

export interface RankedRow {
  key: string
  name: string
  tokens: number
  cost: number
}

const props = withDefaults(defineProps<{
  title: string
  rows: RankedRow[]
  /** Denominator of the % column. */
  total: number
  mono?: boolean
  limit?: number
}>(), { mono: false, limit: 5 })

const { t } = useI18n()
const showAll = ref(false)
const top = computed(() => Math.max(1, ...props.rows.map((r) => r.tokens)))
const shown = computed(() => (showAll.value ? props.rows : props.rows.slice(0, props.limit)))

function fmtCompact(n: number): string {
  if (n >= 1_000_000_000) return (n / 1_000_000_000).toFixed(2) + 'B'
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + 'M'
  if (n >= 1_000) return (n / 1_000).toFixed(1) + 'k'
  return String(n)
}
function fmtCost(n: number): string {
  if (n === 0) return '$0'
  if (n < 0.01) return '$' + n.toFixed(4)
  return '$' + n.toFixed(2)
}
function pct(n: number): string {
  return props.total ? `${Math.round((n / props.total) * 100)}%` : '0%'
}
</script>

<template>
  <Card class="border-border/60 min-w-0" body-class="px-3 pb-2.5 pt-2.5">
    <h2 class="mb-1.5 text-[11px] font-semibold text-muted-foreground">{{ title }}</h2>
    <table class="w-full table-fixed text-xs">
      <thead>
        <tr class="text-[10px] text-muted-foreground">
          <th class="pb-1 text-left font-normal" />
          <th class="w-16 pb-1 text-right font-normal">{{ t('stats.table.tokens') }}</th>
          <th class="w-16 pb-1 text-right font-normal">{{ t('stats.table.cost') }}</th>
          <th class="w-10 pb-1 text-right font-normal">{{ t('stats.table.percent') }}</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="r in shown" :key="r.key" class="border-t border-border/40">
          <td class="py-1 pr-2">
            <span class="block truncate" :class="mono && 'font-mono text-[11px]'" :title="r.name">{{ r.name }}</span>
            <span class="mt-0.5 block h-0.5 overflow-hidden rounded-full bg-muted">
              <span class="block h-full rounded-full bg-primary" :style="{ width: `${(r.tokens / top) * 100}%` }" />
            </span>
          </td>
          <td class="py-1 text-right tabular-nums">{{ fmtCompact(r.tokens) }}</td>
          <td class="py-1 text-right tabular-nums text-muted-foreground">{{ fmtCost(r.cost) }}</td>
          <td class="py-1 text-right tabular-nums text-muted-foreground">{{ pct(r.tokens) }}</td>
        </tr>
      </tbody>
    </table>
    <button
      v-if="rows.length > limit"
      type="button"
      class="mt-1.5 cursor-pointer text-[11px] text-primary hover:underline"
      @click="showAll = !showAll"
    >
      {{ showAll ? t('stats.table.showLess') : t('stats.table.showAll', { n: rows.length }) }}
    </button>
  </Card>
</template>
