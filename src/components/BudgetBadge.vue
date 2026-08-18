<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { AlertTriangle, RefreshCw } from 'lucide-vue-next'
import { useBudgetStore } from '@/stores/budget'
import { useAppSettingsStore } from '@/stores/appSettings'

const { t } = useI18n()
const budget = useBudgetStore()
const app = useAppSettingsStore()
const now = ref(Date.now())

const PERIOD_LABEL = computed<Record<string, string>>(() => ({
  week: t('misc.budget.thisWeek'),
  '5h': t('misc.budget.last5h'),
}))

/** A single provider's verdict flattened for the row renderer. */
interface ProviderView {
  key: 'claude' | 'codex'
  label: string
  source: 'plan' | 'disabled'
  hasPlan: boolean
  enabled: boolean
  period: string
  percent: number
  isWarning: boolean
  isOver: boolean
  reset: string | null
  capturedAt: string | null
  isStale: boolean
  rolledOver: boolean
  hasStatus: boolean
  refreshing: boolean
  refreshError: string | null
  refreshUnavailable: boolean
  onRefresh: () => void
}

const claudeView = computed<ProviderView>(() => ({
  key: 'claude',
  label: 'Claude',
  source: budget.source,
  hasPlan: budget.hasPlan,
  enabled: budget.enabled,
  period: budget.period,
  percent: budget.percent,
  isWarning: budget.isWarning,
  isOver: budget.isOver,
  reset: budget.reset,
  capturedAt: budget.capturedAt,
  isStale: budget.isStale,
  rolledOver: budget.rolledOver,
  hasStatus: budget.status != null,
  refreshing: budget.refreshingPlan,
  refreshError: budget.refreshError,
  refreshUnavailable: budget.refreshUnavailable,
  onRefresh: () => budget.refreshPlanUsage({ reason: 'manual', force: true }),
}))

const codexView = computed<ProviderView>(() => ({
  key: 'codex',
  label: 'Codex',
  source: budget.codexSource,
  hasPlan: budget.codexHasPlan,
  enabled: budget.codexEnabled,
  period: budget.codexPeriod,
  percent: budget.codexPercent,
  isWarning: budget.codexIsWarning,
  isOver: budget.codexIsOver,
  reset: budget.codexReset,
  capturedAt: budget.codexCapturedAt,
  isStale: budget.codexIsStale,
  rolledOver: budget.codexRolledOver,
  hasStatus: budget.codexStatus != null,
  refreshing: budget.refreshingCodexPlan,
  refreshError: budget.codexRefreshError,
  refreshUnavailable: budget.codexRefreshUnavailable,
  onRefresh: () => budget.refreshCodexPlanUsage({ reason: 'manual', force: true }),
}))

const views = computed<ProviderView[]>(() => [claudeView.value, codexView.value])

function resetText(v: ProviderView): string {
  if (!v.reset) return v.period === '5h' && !v.hasPlan ? t('misc.budget.rolling5h') : ''
  const ms = new Date(v.reset).getTime() - now.value
  if (ms <= 0) return t('misc.budget.resetsSoon')
  const totalMinutes = Math.max(1, Math.ceil(ms / 60_000))
  const days = Math.floor(totalMinutes / 1440)
  const hours = Math.floor((totalMinutes % 1440) / 60)
  const minutes = totalMinutes % 60
  if (days > 0) return hours > 0 ? t('misc.budget.resetsInDaysHours', { days, hours }) : t('misc.budget.resetsInDays', { days })
  if (hours > 0) return minutes > 0 ? t('misc.budget.resetsInHoursMins', { hours, minutes }) : t('misc.budget.resetsInHours', { hours })
  return t('misc.budget.resetsInMins', { minutes })
}

function elapsedText(iso: string | null): string {
  if (!iso) return ''
  const ms = Math.max(0, now.value - new Date(iso).getTime())
  const totalMinutes = Math.max(1, Math.floor(ms / 60_000))
  if (totalMinutes < 60) return t('misc.budget.agoMins', { minutes: totalMinutes })
  const hours = Math.floor(totalMinutes / 60)
  const minutes = totalMinutes % 60
  if (hours < 24) return minutes > 0 ? t('misc.budget.agoHoursMins', { hours, minutes }) : t('misc.budget.agoHours', { hours })
  const days = Math.floor(hours / 24)
  return t('misc.budget.agoDays', { days })
}

function tone(v: ProviderView): 'over' | 'warning' | 'stale' | 'ok' | 'neutral' {
  if (!v.hasStatus) return 'neutral'
  // Real usage severity wins — a genuine near/over-limit must always show, even
  // if the last refresh failed.
  if (v.isOver) return 'over'
  if (v.isWarning) return 'warning'
  // A failed refresh with stale data is a data-freshness issue, NOT a usage
  // warning. Render it quietly (no amber, no alert icon) so a low % like 3%
  // doesn't masquerade as "approaching limit".
  if (v.refreshError && !v.refreshUnavailable && v.isStale) return 'stale'
  if (v.enabled) return 'ok'
  return 'neutral'
}

// ── dense-pill presentation ────────────────────────────────────────────────
// Tone → colour classes. Only warning/over tint the whole row (to draw the eye);
// ok/neutral stay quiet and let the dot + bar carry the state.
const TONE_ROW: Record<string, string> = {
  over: 'border-red-500/40 bg-red-500/10 text-red-600 dark:text-red-400',
  warning: 'border-amber-500/40 bg-amber-500/10 text-amber-600 dark:text-amber-400',
  // Stale reads like ok/neutral (quiet) — freshness is conveyed by the tooltip
  // and the refresh button, not by an alarming colour.
  stale: 'border-border/60 bg-muted/30 text-muted-foreground',
  ok: 'border-border/60 bg-muted/30 text-foreground',
  neutral: 'border-border/60 bg-muted/30 text-muted-foreground',
}
const TONE_FILL: Record<string, string> = {
  over: 'bg-red-500',
  warning: 'bg-amber-500',
  stale: 'bg-muted-foreground/40',
  ok: 'bg-indigo-500',
  neutral: 'bg-muted-foreground/40',
}

/** True when we have a real usage % to render as number + bar. */
function hasMeter(v: ProviderView): boolean {
  return v.hasStatus && v.enabled
}

function barWidth(v: ProviderView): string {
  return Math.min(100, Math.max(0, v.percent)) + '%'
}

/** Compact reset label for the dense row, e.g. "2h10m", "3d", "45m". */
function resetShort(v: ProviderView): string {
  if (!v.reset) return ''
  const ms = new Date(v.reset).getTime() - now.value
  if (ms <= 0) return t('misc.budget.soon')
  const totalMinutes = Math.max(1, Math.ceil(ms / 60_000))
  const days = Math.floor(totalMinutes / 1440)
  const hours = Math.floor((totalMinutes % 1440) / 60)
  const minutes = totalMinutes % 60
  if (days > 0) return hours > 0 ? `${days}d${hours}h` : `${days}d`
  if (hours > 0) return minutes > 0 ? `${hours}h${minutes}m` : `${hours}h`
  return `${minutes}m`
}

/** Short text shown in place of the meter when there's no usage % yet. */
function compactStatus(v: ProviderView): string {
  if (v.refreshing) return t('misc.budget.checking')
  if (v.refreshError && !v.refreshUnavailable) return t('misc.budget.refreshFailed')
  return t('misc.budget.noPlanUsage')
}

/** Full context for the hover tooltip, since the dense row hides most of it. */
function tooltip(v: ProviderView): string {
  return `${titleFor(v)} — ${heading(v)} · ${detail(v)}`
}

function heading(v: ProviderView): string {
  if (!v.hasStatus) return v.refreshing ? t('misc.budget.refreshingUsage') : t('misc.budget.usageStatus')
  if (v.hasPlan) {
    if (v.isOver) return t('misc.budget.planLimitReached')
    if (v.rolledOver) return t('misc.budget.planWindowReset')
    const base = t('misc.budget.percentOfPlan', { percent: v.percent })
    return v.isStale && !v.refreshing ? t('misc.budget.cached', { base }) : base
  }
  return t('misc.budget.noPlanUsageYet')
}

function detail(v: ProviderView): string {
  const updatesHint = v.key === 'codex' ? t('misc.budget.updatesCodex') : t('misc.budget.updatesClaude')
  const fetchingHint = v.key === 'codex' ? t('misc.budget.fetchingCodex') : t('misc.budget.fetchingClaude')
  const noData = v.refreshError && !v.refreshUnavailable
    ? t('misc.budget.refreshFailedReason', { error: v.refreshError })
    : t('misc.budget.noPlanCaptured')
  if (!v.hasStatus) return v.refreshing ? fetchingHint : noData
  if (v.hasPlan) {
    const parts = [PERIOD_LABEL.value[v.period] ?? v.period]
    const rt = resetText(v)
    if (rt) parts.push(rt)
    if (v.refreshing) {
      parts.push(t('misc.budget.refreshing'))
    } else if (v.rolledOver) {
      parts.push(t('misc.budget.runOnceToRefresh'))
    } else {
      const captured = elapsedText(v.capturedAt)
      if (captured) parts.push(v.isStale ? t('misc.budget.lastCaptured', { ago: captured }) : t('misc.budget.asOf', { ago: captured }))
      if (v.isStale) parts.push(updatesHint)
    }
    if (v.refreshError && !v.refreshUnavailable) parts.push(t('misc.budget.refreshFailedReason', { error: v.refreshError }))
    return parts.join(' · ')
  }
  return v.refreshing ? fetchingHint : noData
}

function titleFor(v: ProviderView): string {
  if (v.hasPlan) return t('misc.budget.planUsageTitle', { label: v.label, period: PERIOD_LABEL.value[v.period] ?? v.period })
  return t('misc.budget.usageStatusTitle', { label: v.label })
}

let timer: ReturnType<typeof setInterval> | null = null
let clockTimer: ReturnType<typeof setInterval> | null = null
let unlistenPlanUsage: UnlistenFn | null = null
let unlistenBudgetStatus: UnlistenFn | null = null

onMounted(async () => {
  await app.ensureLoaded()
  now.value = Date.now()
  await Promise.all([budget.refresh(), budget.refreshCodex()])
  // The only automatic probes: one per provider on startup. Afterwards the % is
  // kept fresh by the piggybacked capture on every run (Claude /usage, Codex
  // rate-limits) or by the manual refresh button — no background polling.
  budget.refreshPlanUsage({ reason: 'startup', force: true })
  budget.refreshCodexPlanUsage({ reason: 'startup', force: true })
  // Local IPC only (no engine call): recompute is_stale / rolled_over against
  // the clock, and pick up snapshots written by runs.
  timer = setInterval(() => {
    budget.refresh()
    budget.refreshCodex()
  }, 60_000)
  clockTimer = setInterval(() => { now.value = Date.now() }, 30_000)
  // Immediately when a sidecar / watcher persists a fresh snapshot during a run.
  unlistenPlanUsage = await listen<{ provider?: string }>('plan_usage_updated', (e) => {
    if (e.payload?.provider === 'codex') {
      if (!budget.refreshingCodexPlan) budget.refreshCodex()
    } else if (!budget.refreshingPlan) {
      budget.refresh()
    }
  })
  unlistenBudgetStatus = await listen('budget_status_updated', () => {
    budget.refresh()
    budget.refreshCodex()
  })
})

onUnmounted(() => {
  if (timer) clearInterval(timer)
  if (clockTimer) clearInterval(clockTimer)
  if (unlistenPlanUsage) unlistenPlanUsage()
  if (unlistenBudgetStatus) unlistenBudgetStatus()
})
// The badge shows real plan usage only, so it no longer reacts to the Usage
// budget settings — those only gate run-blocking. Fresh % arrives via the
// per-run snapshot capture, the startup probe, and the 60s poll above.
</script>

<template>
  <div class="mx-2 mb-2 space-y-1">
    <div
      v-for="v in views"
      :key="v.key"
      class="group relative flex items-center gap-1.5 rounded-md border px-2 py-1 text-[10px] leading-none transition-colors"
      :class="TONE_ROW[tone(v)]"
      :title="tooltip(v)"
    >
      <!-- status indicator: alert icon when warning/over -->
      <AlertTriangle
        v-if="tone(v) === 'over' || tone(v) === 'warning'"
        class="h-3 w-3 shrink-0"
        :stroke-width="2.5"
      />

      <!-- provider label -->
      <span class="shrink-0 font-medium opacity-70">{{ v.label }}</span>

      <!-- meter: percent + bar when a real usage % exists -->
      <template v-if="hasMeter(v)">
        <span class="w-8 shrink-0 text-right font-mono font-semibold tabular-nums">{{ v.percent }}%</span>
        <div class="h-1.5 flex-1 overflow-hidden rounded-full bg-black/10 dark:bg-white/10">
          <div
            class="h-full rounded-full transition-[width] duration-300 motion-reduce:transition-none"
            :class="TONE_FILL[tone(v)]"
            :style="{ width: barWidth(v) }"
          />
        </div>
      </template>
      <!-- otherwise a muted status filling the meter area -->
      <span v-else class="flex-1 truncate font-mono opacity-60">{{ compactStatus(v) }}</span>

      <!-- right slot: reset time; swaps to the refresh button on hover / while refreshing -->
      <div class="relative h-3 w-11 shrink-0">
        <span
          class="absolute inset-0 flex items-center justify-end font-mono tabular-nums opacity-60 transition-opacity"
          :class="v.refreshing ? 'opacity-0' : 'group-hover:opacity-0'"
        >{{ hasMeter(v) ? resetShort(v) : '' }}</span>
        <button
          type="button"
          class="absolute inset-0 flex items-center justify-end rounded transition-opacity focus-visible:opacity-100 focus-visible:outline-none"
          :class="v.refreshing ? 'opacity-100' : 'cursor-pointer opacity-0 group-hover:opacity-100'"
          :disabled="v.refreshing"
          :aria-label="t('misc.budget.refreshUsage', { label: v.label })"
          :title="t('misc.budget.refreshUsageTitle', { label: v.label })"
          @click="v.onRefresh"
        >
          <RefreshCw class="h-3 w-3" :class="{ 'animate-spin': v.refreshing }" :stroke-width="2" />
        </button>
      </div>
    </div>
  </div>
</template>
