<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { AlertTriangle, CheckCircle2, Gauge, RefreshCw } from 'lucide-vue-next'
import { useBudgetStore } from '@/stores/budget'
import { useAppSettingsStore } from '@/stores/appSettings'
import { formatTokensShort } from '@/lib/contextLimits'

const budget = useBudgetStore()
const app = useAppSettingsStore()
const now = ref(Date.now())

const PERIOD_LABEL: Record<string, string> = {
  month: 'this month',
  week: 'this week',
  '5h': 'last 5h',
}

const resetText = computed(() => {
  if (!budget.reset) return budget.period === '5h' && !budget.hasPlan ? 'rolling 5h window' : ''
  const ms = new Date(budget.reset).getTime() - now.value
  if (ms <= 0) return 'resets soon'
  const totalMinutes = Math.max(1, Math.ceil(ms / 60_000))
  const days = Math.floor(totalMinutes / 1440)
  const hours = Math.floor((totalMinutes % 1440) / 60)
  const minutes = totalMinutes % 60
  if (days > 0) return hours > 0 ? `resets in ${days}d ${hours}h` : `resets in ${days}d`
  if (hours > 0) return minutes > 0 ? `resets in ${hours}h ${minutes}m` : `resets in ${hours}h`
  return `resets in ${minutes}m`
})

function elapsedText(iso: string | null): string {
  if (!iso) return ''
  const ms = Math.max(0, now.value - new Date(iso).getTime())
  const totalMinutes = Math.max(1, Math.floor(ms / 60_000))
  if (totalMinutes < 60) return `${totalMinutes}m ago`
  const hours = Math.floor(totalMinutes / 60)
  const minutes = totalMinutes % 60
  if (hours < 24) return minutes > 0 ? `${hours}h ${minutes}m ago` : `${hours}h ago`
  const days = Math.floor(hours / 24)
  return `${days}d ago`
}

const tone = computed(() => {
  if (!budget.status) return 'neutral'
  if (budget.refreshError && !budget.refreshUnavailable && budget.isStale) return 'warning'
  if (budget.isOver) return 'over'
  if (budget.isWarning) return 'warning'
  if (budget.enabled) return 'ok'
  return 'neutral'
})

const icon = computed(() => {
  if (budget.isOver || budget.isWarning) return AlertTriangle
  if (budget.enabled) return CheckCircle2
  return Gauge
})

const heading = computed(() => {
  if (!budget.status) return budget.refreshingPlan ? 'Refreshing usage' : 'Usage status'
  if (budget.hasPlan) {
    if (budget.isOver) return 'Plan limit reached'
    if (budget.rolledOver) return 'Plan window reset'
    const base = `${budget.percent}% of plan limit`
    // A stale snapshot is still worth showing, but flag it as cached so the
    // number isn't mistaken for live plan usage.
    return budget.isStale && !budget.refreshingPlan ? `Cached: ${base}` : base
  }
  if (budget.source === 'tokens') {
    if (budget.isOver) return 'Over token budget'
    return `${budget.percent}% of token budget`
  }
  return 'Usage status'
})

const detail = computed(() => {
  if (!budget.status) return budget.refreshingPlan ? 'Fetching Claude /usage' : 'No plan usage captured yet'
  if (budget.hasPlan) {
    const parts = [PERIOD_LABEL[budget.period] ?? budget.period]
    if (resetText.value) parts.push(resetText.value)
    if (budget.refreshingPlan) {
      parts.push('refreshing…')
    } else if (budget.rolledOver) {
      parts.push('run once to refresh the new window')
    } else {
      // Always show how fresh the % is; % only updates during Claude runs or a
      // manual refresh (no background polling), so surfacing the age is honest.
      const captured = elapsedText(budget.capturedAt)
      if (captured) parts.push(budget.isStale ? `last captured ${captured}` : `as of ${captured}`)
      if (budget.isStale) parts.push('updates during Claude runs')
    }
    if (budget.refreshError && !budget.refreshUnavailable) parts.push(`refresh failed: ${budget.refreshError}`)
    return parts.join(' · ')
  }
  if (budget.source === 'tokens') {
    const parts = [`${formatTokensShort(budget.usedTokens)} / ${formatTokensShort(budget.limit)}`]
    if (resetText.value) parts.push(resetText.value)
    return parts.join(' · ')
  }
  return 'No plan usage captured yet'
})

function manualRefresh() {
  budget.refreshPlanUsage({ reason: 'manual', force: true })
}

let timer: ReturnType<typeof setInterval> | null = null
let clockTimer: ReturnType<typeof setInterval> | null = null
let unlistenPlanUsage: UnlistenFn | null = null
let unlistenBudgetStatus: UnlistenFn | null = null

onMounted(async () => {
  await app.ensureLoaded()
  now.value = Date.now()
  await budget.refresh()
  // The ONLY automatic probe to Claude: one on startup. Afterwards the % is
  // kept fresh for free by the piggybacked /usage capture on every run, or by
  // the manual refresh button — no background polling (avoids wasting tokens
  // and rate-limit requests).
  budget.refreshPlanUsage({ reason: 'startup', force: true })
  // Local IPC only (no Claude call): recompute is_stale / rolled_over against
  // the clock, and pick up snapshots written by runs.
  timer = setInterval(() => budget.refresh(), 60_000)
  clockTimer = setInterval(() => { now.value = Date.now() }, 30_000)
  // Immediately when the sidecar persists a fresh snapshot during a run.
  unlistenPlanUsage = await listen('plan_usage_updated', () => {
    if (!budget.refreshingPlan) budget.refresh()
  })
  // Also refresh when a turn's local usage ledger is written. This keeps the
  // fallback token-budget warning live even when no Claude plan snapshot changes.
  unlistenBudgetStatus = await listen('budget_status_updated', () => budget.refresh())
})

onUnmounted(() => {
  if (timer) clearInterval(timer)
  if (clockTimer) clearInterval(clockTimer)
  if (unlistenPlanUsage) unlistenPlanUsage()
  if (unlistenBudgetStatus) unlistenBudgetStatus()
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
    class="mx-2 mb-2 rounded-md border px-2.5 py-1.5 text-[10px] leading-tight"
    :class="{
      'border-red-500/40 bg-red-500/10 text-red-600 dark:text-red-400': tone === 'over',
      'border-amber-500/40 bg-amber-500/10 text-amber-600 dark:text-amber-400': tone === 'warning',
      'border-emerald-500/30 bg-emerald-500/10 text-emerald-700 dark:text-emerald-400': tone === 'ok',
      'border-border/70 bg-muted/35 text-muted-foreground': tone === 'neutral',
    }"
    :title="budget.hasPlan
      ? `Subscription plan usage · ${PERIOD_LABEL[budget.period] ?? budget.period}`
      : budget.source === 'tokens'
        ? `Token budget ${PERIOD_LABEL[budget.period] ?? budget.period}`
        : 'Usage status'"
  >
    <div class="flex items-center gap-1.5 font-medium">
      <component :is="icon" class="h-3 w-3 shrink-0" :stroke-width="2" />
      <span>{{ heading }}</span>
      <button
        type="button"
        class="ml-auto shrink-0 rounded p-0.5 opacity-60 transition hover:opacity-100 disabled:opacity-40"
        :disabled="budget.refreshingPlan"
        title="Refresh plan usage now"
        @click="manualRefresh"
      >
        <RefreshCw class="h-3 w-3" :class="{ 'animate-spin': budget.refreshingPlan }" :stroke-width="2" />
      </button>
    </div>
    <div class="mt-0.5 font-mono opacity-80">{{ detail }}</div>
  </div>
</template>
