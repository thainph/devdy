<script setup lang="ts">
import { computed, onMounted, onUnmounted, watch } from 'vue'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { AlertTriangle } from 'lucide-vue-next'
import { useBudgetStore } from '@/stores/budget'
import { useAppSettingsStore } from '@/stores/appSettings'
import { formatTokensShort } from '@/lib/contextLimits'

const budget = useBudgetStore()
const app = useAppSettingsStore()

const PERIOD_LABEL: Record<string, string> = {
  month: 'this month',
  week: 'this week',
  '5h': 'last 5h',
}

const resetText = computed(() => {
  if (!budget.reset) return budget.period === '5h' && !budget.hasPlan ? 'rolling 5h window' : ''
  const ms = new Date(budget.reset).getTime() - Date.now()
  if (ms <= 0) return 'resets soon'
  const hours = ms / 3_600_000
  if (hours >= 48) return `resets in ${Math.round(hours / 24)}d`
  if (hours >= 1) return `resets in ${Math.round(hours)}h`
  return `resets in ${Math.max(1, Math.round(ms / 60_000))}m`
})

// Show whenever real plan data OR the self-imposed budget crosses a threshold.
const visible = computed(() => (budget.hasPlan || budget.enabled) && (budget.isWarning || budget.isOver))

let timer: ReturnType<typeof setInterval> | null = null
let unlisten: UnlistenFn | null = null

onMounted(async () => {
  await app.ensureLoaded()
  budget.refresh()
  // Refresh periodically so the rolling window and post-run totals stay live.
  timer = setInterval(() => budget.refresh(), 60_000)
  // And immediately when the sidecar captures a fresh /usage snapshot.
  unlisten = await listen('plan_usage_updated', () => budget.refresh())
})

onUnmounted(() => {
  if (timer) clearInterval(timer)
  if (unlisten) unlisten()
})

// Refetch when the configured budget settings change (period/limit/threshold
// alter the verdict). SettingsView refreshes the app-settings store on save.
watch(
  () => [
    app.settings?.token_budget_period,
    app.settings?.token_budget_limit,
    app.settings?.budget_warn_percent,
  ],
  () => budget.refresh(),
)
</script>

<template>
  <div
    v-if="visible"
    class="mx-2 mb-2 rounded-md border px-2.5 py-1.5 text-[10px] leading-tight"
    :class="budget.isOver
      ? 'border-red-500/40 bg-red-500/10 text-red-600 dark:text-red-400'
      : 'border-amber-500/40 bg-amber-500/10 text-amber-600 dark:text-amber-400'"
    :title="budget.hasPlan
      ? `Subscription plan usage · ${PERIOD_LABEL[budget.period] ?? budget.period}`
      : `Token budget ${PERIOD_LABEL[budget.period] ?? budget.period}`"
  >
    <div class="flex items-center gap-1.5 font-medium">
      <AlertTriangle class="h-3 w-3 shrink-0" :stroke-width="2" />
      <span v-if="budget.hasPlan">{{ budget.isOver ? 'Plan limit reached' : `${budget.percent}% of plan limit` }}</span>
      <span v-else>{{ budget.isOver ? 'Over token budget' : `${budget.percent}% of token budget` }}</span>
    </div>
    <!-- Real plan data: show % + reset only (no self-set token cap to compare). -->
    <div v-if="budget.hasPlan" class="mt-0.5 font-mono opacity-80">
      {{ PERIOD_LABEL[budget.period] ?? budget.period }}
      <span v-if="resetText"> · {{ resetText }}</span>
    </div>
    <!-- Estimate mode: local token count against the configured limit. -->
    <div v-else class="mt-0.5 font-mono opacity-80">
      {{ formatTokensShort(budget.usedTokens) }} / {{ formatTokensShort(budget.limit) }}
      <span v-if="resetText"> · {{ resetText }}</span>
    </div>
  </div>
</template>
