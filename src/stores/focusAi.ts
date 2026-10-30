import { defineStore } from 'pinia'
import { ref } from 'vue'
import { emit } from '@tauri-apps/api/event'
import { invoke } from '@/lib/tauri'
import { WINDOW_OPEN_RUN, type WindowOpenRun } from '@/lib/sessionLinks'
import type { AiAnalysis, AiAnalyzeResult, AiSuggestion, DayPlan } from '@/lib/focus'

/**
 * Focus AI (docs/focus-board-spec.md §3.6, phase P3) — every call is a button
 * press: suggest a todo from its source, analyze a todo, plan today, push the
 * plan to Google Calendar, start a session on a todo. Requests carry an id so
 * they can be cancelled; `running` maps a key (`todo:<id>`, `plan`,
 * `suggest:<url>`) to it for the spinners. Failures throw (the caller toasts)
 * and change nothing (BR-09).
 */
export const useFocusAiStore = defineStore('focusAi', () => {
  const running = ref<Record<string, string>>({})
  const plan = ref<DayPlan | null>(null)
  /** Latest structured analysis per todo this session (the stored `ai_plan` is its markdown). */
  const analyses = ref<Record<string, AiAnalysis>>({})

  async function request<T>(key: string, command: string, args: Record<string, unknown>): Promise<T> {
    if (running.value[key]) throw new Error('busy: an AI request for this is already running')
    const requestId = crypto.randomUUID()
    running.value = { ...running.value, [key]: requestId }
    try {
      return await invoke<T>(command, { ...args, requestId })
    } finally {
      const next = { ...running.value }
      delete next[key]
      running.value = next
    }
  }

  const isRunning = (key: string) => !!running.value[key]

  async function cancel(key: string) {
    const requestId = running.value[key]
    if (requestId) await invoke<boolean>('focus_ai_cancel', { requestId })
  }

  const suggest = (sourceUrl: string, projectId: string | null, title: string | null) =>
    request<AiSuggestion>(`suggest:${sourceUrl}`, 'focus_ai_suggest', { sourceUrl, projectId, title })

  async function analyze(todoId: string) {
    const r = await request<AiAnalyzeResult>(`todo:${todoId}`, 'focus_ai_analyze', { todoId })
    analyses.value = { ...analyses.value, [todoId]: r.analysis }
    return r
  }

  async function loadPlan() {
    plan.value = await invoke<DayPlan | null>('focus_day_plan_get', { date: null })
  }

  async function planToday() {
    plan.value = await request<DayPlan>('plan', 'focus_ai_plan_today', {})
    return plan.value
  }

  /** All not-yet-pushed blocks, or `blockIds`. Partial success is kept (reloaded) even when it throws. */
  async function planToCalendar(blockIds?: string[]) {
    if (!plan.value) return
    try {
      plan.value = await invoke<DayPlan>('focus_plan_to_calendar', { date: plan.value.date, blockIds: blockIds ?? null })
    } catch (e) {
      await loadPlan().catch(() => {})
      throw e
    }
  }

  /** New session on the todo (throws `project_required: …` without a project), then open it. */
  async function startWithAi(todoId: string, projectId: string) {
    const runId = await invoke<string>('focus_start_with_ai', { todoId })
    await emit(WINDOW_OPEN_RUN, { projectId, runId } satisfies WindowOpenRun)
    return runId
  }

  /** BR-11: linked sessions' time, else working time from start to completion. */
  const suggestActual = (todoId: string) => invoke<number | null>('focus_suggest_actual', { todoId })

  return { running, plan, analyses, isRunning, cancel, suggest, analyze, loadPlan, planToday, planToCalendar, startWithAi, suggestActual }
})
