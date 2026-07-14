import { defineStore } from 'pinia'
import { ref, computed } from 'vue'
import { invoke } from '@/lib/tauri'

type PlanRefreshReason = 'startup' | 'manual'

interface PlanRefreshOptions {
  reason?: PlanRefreshReason
  force?: boolean
}

const FAILED_REFRESH_BACKOFF_MS = 5 * 60_000

/**
 * One budget verdict, computed entirely by the backend (`get_budget_status`).
 * Prefers real subscription plan utilization (`/usage`); falls back to the
 * self-imposed token budget; otherwise disabled. The badge and the run-blocking
 * logic read the SAME verdict, so they can never diverge.
 */
interface BudgetStatus {
  source: 'plan' | 'tokens' | 'disabled'
  period: string
  percent: number
  is_warning: boolean
  is_over: boolean
  used_tokens: number
  limit_tokens: number
  reset: string | null
  captured_at: string | null
  is_stale: boolean
  status: 'allowed' | 'warning' | 'blocked' | null
  rolled_over: boolean
}

export const useBudgetStore = defineStore('budget', () => {
  const status = ref<BudgetStatus | null>(null)
  const refreshingPlan = ref(false)
  const refreshError = ref<string | null>(null)
  const startupProbeDone = ref(false)
  const lastFailedPlanProbeAt = ref(0)

  const source = computed(() => status.value?.source ?? 'disabled')
  /** True when accurate subscription plan data backs the verdict (not estimate). */
  const hasPlan = computed(() => source.value === 'plan')
  /** Any guardrail active (plan window or self-imposed token budget). */
  const enabled = computed(() => source.value !== 'disabled')
  const period = computed(() => status.value?.period ?? 'month')
  const percent = computed(() => status.value?.percent ?? 0)
  const isWarning = computed(() => status.value?.is_warning ?? false)
  const isOver = computed(() => status.value?.is_over ?? false)
  const reset = computed(() => status.value?.reset ?? null)
  const usedTokens = computed(() => status.value?.used_tokens ?? 0)
  const limit = computed(() => status.value?.limit_tokens ?? 0)
  const capturedAt = computed(() => status.value?.captured_at ?? null)
  const isStale = computed(() => status.value?.is_stale ?? false)
  const rolledOver = computed(() => status.value?.rolled_over ?? false)
  const refreshUnavailable = computed(() =>
    refreshError.value?.includes('without a fresh usage snapshot') ?? false
  )

  async function refresh() {
    try {
      status.value = await invoke<BudgetStatus>('get_budget_status')
    } catch {
      // leave previous value on transient errors
    }
  }

  async function refreshPlanUsage(options: PlanRefreshOptions = {}) {
    const reason = options.reason ?? 'manual'
    if (reason === 'startup') {
      if (startupProbeDone.value && !options.force) return
      startupProbeDone.value = true
    }
    if (refreshingPlan.value) return

    // After a failed probe, back off before trying again (best-effort probe,
    // don't hammer Claude). `force` (manual button) bypasses the backoff.
    const now = Date.now()
    if (
      !options.force &&
      lastFailedPlanProbeAt.value > 0 &&
      now - lastFailedPlanProbeAt.value < FAILED_REFRESH_BACKOFF_MS
    ) {
      return
    }

    refreshingPlan.value = true
    refreshError.value = null
    try {
      await invoke('refresh_plan_usage')
      lastFailedPlanProbeAt.value = 0
    } catch (e) {
      refreshError.value = String(e)
      lastFailedPlanProbeAt.value = Date.now()
      // Probe is best-effort; fall back to the latest stored snapshot.
    } finally {
      refreshingPlan.value = false
      await refresh()
    }
  }

  return {
    status,
    refreshingPlan,
    refreshError,
    source,
    hasPlan,
    enabled,
    period,
    percent,
    isWarning,
    isOver,
    reset,
    usedTokens,
    limit,
    capturedAt,
    isStale,
    rolledOver,
    refreshUnavailable,
    refresh,
    refreshPlanUsage,
  }
})
