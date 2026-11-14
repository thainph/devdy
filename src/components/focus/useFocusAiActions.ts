// AI actions of a todo (spec §3.6 d) with their toasts: 🚀 Start with AI (a
// new session in the todo's project, then opened), plus the shared error toast
// of ✨ Suggest. A todo without a project asks for one first — FocusAiDialogs
// renders that picker once for every row.
import { ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useToast } from '@/composables/useToast'
import { useFocusStore } from '@/stores/focus'
import { useFocusAiStore } from '@/stores/focusAi'
import { aiErrorCode, type FocusTodo } from '@/lib/focus'

/** "Pick a project" before starting a session on a project-less todo. */
const pickProjectFor = ref<FocusTodo | null>(null)

export function useFocusAiActions() {
  const { t, te } = useI18n()
  const { toast } = useToast()
  const store = useFocusStore()
  const ai = useFocusAiStore()

  /** A command error in the user's language (cancel is silent). */
  function errorText(e: unknown): string | null {
    const { code, detail } = aiErrorCode(e)
    if (code === 'cancelled') return null
    if (code && te(`focus.ai.errors.${code}`)) return t(`focus.ai.errors.${code}`, { detail })
    return t('focus.ai.errors.unknown', { detail: code ? `${code}: ${detail}` : detail })
  }

  function fail(e: unknown) {
    const msg = errorText(e)
    if (msg) toast.error(msg)
  }

  async function startWithAi(todo: FocusTodo) {
    if (!todo.project_id) {
      pickProjectFor.value = todo
      return
    }
    try {
      await ai.startWithAi(todo.id, todo.project_id)
      toast.success(t('focus.ai.started'))
    } catch (e) {
      if (aiErrorCode(e).code === 'project_required') pickProjectFor.value = todo
      else fail(e)
    }
  }

  /** The picker's answer: file the todo under `projectId`, then start. */
  async function startInProject(todo: FocusTodo, projectId: string) {
    pickProjectFor.value = null
    try {
      const updated = await store.update(todo.id, { project_id: projectId })
      await startWithAi(updated)
    } catch (e) {
      fail(e)
    }
  }

  return { startWithAi, startInProject, pickProjectFor, errorText, fail }
}
