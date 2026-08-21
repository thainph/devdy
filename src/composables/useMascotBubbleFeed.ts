// Aggregates the app's transient signals into the fox's speech bubble:
//   • live-run phase (busy / permission / done) → status chatter
//   • toasts (success / error / info) → mirrored into the bubble
// Calendar reminders push directly from CalendarReminder. Runs ONCE in the main
// window (CyberFoxHost), so it must not be instantiated more than once.
import { computed, watch, type Ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useToast } from '@/composables/useToast'
import { useMascotBubble, type MascotBubbleVariant } from '@/composables/useMascotBubble'
import { pickMascotVoice } from '@/composables/useMascotSound'
import type { CyberFoxState } from '@/composables/useMascotState'

// Collapse the fine-grained phase into the few states worth "saying". thinking
// and loading both read as "busy" so the bubble doesn't flicker between them
// while a run streams.
type Coarse = 'idle' | 'busy' | 'permission' | 'success' | 'error'

function coarsePhaseOf(state: CyberFoxState): Coarse {
  switch (state) {
    case 'permission':
      return 'permission'
    case 'thinking':
    case 'loading':
      return 'busy'
    case 'success':
      return 'success'
    case 'error':
      return 'error'
    default:
      return 'idle'
  }
}

export function useMascotBubbleFeed(displayState: Ref<CyberFoxState>) {
  const { t, locale } = useI18n()
  const { state: toastState } = useToast()
  const { push } = useMascotBubble()

  // --- live-run phase → status chatter ---------------------------------
  const coarse = computed(() => coarsePhaseOf(displayState.value))

  // Say a canned phase line, preferring a recorded voice take (clip + matching
  // words) so the bubble text mirrors what the fox actually says; fall back to
  // the generic i18n string when no clip is installed for that variant.
  function sayPhase(variant: MascotBubbleVariant, fallbackKey: string) {
    const line = pickMascotVoice(variant, String(locale.value))
    push(line?.text ?? t(fallbackKey), variant, undefined, line?.clip)
  }

  watch(coarse, (phase, prev) => {
    if (phase === prev) return
    switch (phase) {
      case 'busy':
        sayPhase('thinking', 'mascot.bubble.busy')
        break
      case 'permission':
        sayPhase('permission', 'mascot.bubble.permission')
        break
      case 'success':
        sayPhase('success', 'mascot.bubble.success')
        break
      case 'error':
        sayPhase('error', 'mascot.bubble.error')
        break
      // idle → say nothing (let the last bubble auto-hide)
    }
  })

  // --- toasts → mirror into the bubble ---------------------------------
  const TOAST_VARIANT: Record<string, MascotBubbleVariant> = {
    success: 'success',
    error: 'error',
    info: 'info',
  }
  let lastToastId = 0
  watch(
    () => toastState.items.map((i) => i.id),
    () => {
      // Push every not-yet-seen toast (usually just the newest one).
      for (const item of toastState.items) {
        if (item.id <= lastToastId) continue
        lastToastId = item.id
        push(item.message, TOAST_VARIANT[item.variant] ?? 'info')
      }
    },
    { deep: true },
  )
}
