import { defineStore } from 'pinia'
import { ref, computed } from 'vue'
import { invoke } from '@/lib/tauri'

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
}

export const useBudgetStore = defineStore('budget', () => {
  const status = ref<BudgetStatus | null>(null)

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

  async function refresh() {
    try {
      status.value = await invoke<BudgetStatus>('get_budget_status')
    } catch {
      // leave previous value on transient errors
    }
  }

  return {
    status,
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
    refresh,
  }
})
