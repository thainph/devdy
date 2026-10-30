// Row / drawer actions of the Focus board (spec §3.2–3.3) with their toasts and
// confirms: done (then "how long did it take?" when it was started and estimated),
// undone, start, waiting, snooze presets / custom time, delete.
//
// The two follow-up dialogs (actual time, custom snooze) are module-scope state
// rendered once by FocusView (FocusDialogs), so every row shares them.
import { ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { openUrl } from '@tauri-apps/plugin-opener'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { useFocusStore } from '@/stores/focus'
import { useFocusAiStore } from '@/stores/focusAi'
import { firstLine } from '@/lib/itemText'
import { formatEstimate, type FocusTodo } from '@/lib/focus'

/** "How long did it take?" after Done (§3.3). */
const actualPrompt = ref<{ todo: FocusTodo; suggestion: string } | null>(null)
/** "Snooze until…" with a picked date-time. */
const customSnooze = ref<FocusTodo | null>(null)

export function useFocusActions() {
  const { t, locale } = useI18n()
  const { toast } = useToast()
  const { confirm } = useConfirm()
  const store = useFocusStore()

  async function run(action: () => Promise<unknown>, success?: string): Promise<boolean> {
    try {
      await action()
      if (success) toast.success(success)
      return true
    } catch (e) {
      toast.error(String(e))
      return false
    }
  }

  async function done(todo: FocusTodo) {
    if (!(await run(() => store.markDone(todo.id)))) return
    if (todo.started_at && todo.estimate_min) {
      // BR-11, computed by the backend (linked sessions, else working time).
      const s = await useFocusAiStore().suggestActual(todo.id).catch(() => null)
      actualPrompt.value = { todo, suggestion: s ? formatEstimate(s) : '' }
    }
  }

  const undone = (todo: FocusTodo) => run(() => store.markUndone(todo.id))
  const start = (todo: FocusTodo) => run(() => store.startTodo(todo.id))
  const backToTodo = (todo: FocusTodo) => run(() => store.backToTodo(todo.id))
  const waiting = (todo: FocusTodo) => run(() => store.setWaiting(todo.id))

  function timeText(iso: string): string {
    return new Date(iso).toLocaleString(locale.value, { weekday: 'short', day: '2-digit', month: '2-digit', hour: '2-digit', minute: '2-digit' })
  }

  const snooze = (todo: FocusTodo, until: string | null) =>
    run(() => store.snooze(todo.id, until), until ? t('focus.toast.snoozed', { time: timeText(until) }) : undefined)

  async function remove(todo: FocusTodo): Promise<boolean> {
    const ok = await confirm({
      title: t('focus.confirmDelete.title'),
      message: t('focus.confirmDelete.message', { text: firstLine(todo.text) }),
      confirmLabel: t('focus.confirmDelete.button'),
      cancelLabel: t('common.cancel'),
    })
    return ok ? run(() => store.remove(todo.id), t('focus.toast.deleted')) : false
  }

  function openSource(todo: FocusTodo) {
    if (todo.source_url) void openUrl(todo.source_url).catch(() => {})
  }

  return { done, undone, start, backToTodo, waiting, snooze, remove, openSource, timeText, actualPrompt, customSnooze }
}
