import { defineStore } from 'pinia'
import { ref } from 'vue'
import { emit } from '@tauri-apps/api/event'
import { invoke } from '@/lib/tauri'
import { WINDOW_OPEN_RUN, type WindowOpenRun } from '@/lib/sessionLinks'
import type { AiSuggestion } from '@/lib/focus'

/**
 * Focus AI (docs/focus-board-spec.md §3.6, phase P3) — every call is a button
 * press: suggest a todo from its source, start a session on a todo. Requests
 * carry an id so they can be cancelled; `running` maps a key
 * (`suggest:<url>`) to it for the spinners. Failures throw (the caller toasts)
 * and change nothing (BR-09).
 */
export const useFocusAiStore = defineStore('focusAi', () => {
  const running = ref<Record<string, string>>({})

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

  /** New session on the todo (throws `project_required: …` without a project), then open it. */
  async function startWithAi(todoId: string, projectId: string) {
    const runId = await invoke<string>('focus_start_with_ai', { todoId })
    await emit(WINDOW_OPEN_RUN, { projectId, runId } satisfies WindowOpenRun)
    return runId
  }

  return { running, isRunning, cancel, suggest, startWithAi }
})
