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
  source: 'plan' | 'disabled'
  period: string
  percent: number
  is_warning: boolean
  is_over: boolean
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

  // Codex plan verdict (second badge row) — populated from the separate
  // `plan_usage_codex` snapshot via `get_codex_budget_status`.
  const codexStatus = ref<BudgetStatus | null>(null)
  const refreshingCodexPlan = ref(false)
  const codexRefreshError = ref<string | null>(null)
  const codexStartupProbeDone = ref(false)
  const lastFailedCodexProbeAt = ref(0)

  const source = computed(() => status.value?.source ?? 'disabled')
  /** True when accurate subscription plan data backs the verdict. */
  const hasPlan = computed(() => source.value === 'plan')
  /** True when a plan window backs the verdict (badge has a % to show). */
  const enabled = computed(() => source.value !== 'disabled')
  const period = computed(() => status.value?.period ?? 'week')
  const percent = computed(() => status.value?.percent ?? 0)
  const isWarning = computed(() => status.value?.is_warning ?? false)
  const isOver = computed(() => status.value?.is_over ?? false)
  const reset = computed(() => status.value?.reset ?? null)
  const capturedAt = computed(() => status.value?.captured_at ?? null)
  const isStale = computed(() => status.value?.is_stale ?? false)
  const rolledOver = computed(() => status.value?.rolled_over ?? false)
  const refreshUnavailable = computed(() =>
    refreshError.value?.includes('without a fresh usage snapshot') ?? false
  )

  // Codex mirrors of the above.
  const codexSource = computed(() => codexStatus.value?.source ?? 'disabled')
  const codexHasPlan = computed(() => codexSource.value === 'plan')
  const codexEnabled = computed(() => codexSource.value !== 'disabled')
  const codexPeriod = computed(() => codexStatus.value?.period ?? 'week')
  const codexPercent = computed(() => codexStatus.value?.percent ?? 0)
  const codexIsWarning = computed(() => codexStatus.value?.is_warning ?? false)
  const codexIsOver = computed(() => codexStatus.value?.is_over ?? false)
  const codexReset = computed(() => codexStatus.value?.reset ?? null)
  const codexCapturedAt = computed(() => codexStatus.value?.captured_at ?? null)
  const codexIsStale = computed(() => codexStatus.value?.is_stale ?? false)
  const codexRolledOver = computed(() => codexStatus.value?.rolled_over ?? false)
  const codexRefreshUnavailable = computed(() =>
    codexRefreshError.value?.includes('without a fresh usage snapshot') ?? false
  )

  async function refresh() {
    try {
      status.value = await invoke<BudgetStatus>('get_budget_status')
    } catch {
      // leave previous value on transient errors
    }
  }

  async function refreshCodex() {
    try {
      codexStatus.value = await invoke<BudgetStatus>('get_codex_budget_status')
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

  async function refreshCodexPlanUsage(options: PlanRefreshOptions = {}) {
    const reason = options.reason ?? 'manual'
    if (reason === 'startup') {
      if (codexStartupProbeDone.value && !options.force) return
      codexStartupProbeDone.value = true
    }
    if (refreshingCodexPlan.value) return

    const now = Date.now()
    if (
      !options.force &&
      lastFailedCodexProbeAt.value > 0 &&
      now - lastFailedCodexProbeAt.value < FAILED_REFRESH_BACKOFF_MS
    ) {
      return
    }

    refreshingCodexPlan.value = true
    codexRefreshError.value = null
    try {
      await invoke('refresh_codex_plan_usage')
      lastFailedCodexProbeAt.value = 0
    } catch (e) {
      codexRefreshError.value = String(e)
      lastFailedCodexProbeAt.value = Date.now()
    } finally {
      refreshingCodexPlan.value = false
      await refreshCodex()
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
    capturedAt,
    isStale,
    rolledOver,
    refreshUnavailable,
    refresh,
    refreshPlanUsage,
    // Codex
    codexStatus,
    refreshingCodexPlan,
    codexRefreshError,
    codexSource,
    codexHasPlan,
    codexEnabled,
    codexPeriod,
    codexPercent,
    codexIsWarning,
    codexIsOver,
    codexReset,
    codexCapturedAt,
    codexIsStale,
    codexRolledOver,
    codexRefreshUnavailable,
    refreshCodex,
    refreshCodexPlanUsage,
  }
})
