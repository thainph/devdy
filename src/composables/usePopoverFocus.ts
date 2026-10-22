// Focus handling for a hand-rolled popover (trigger + panel inside one wrapper,
// like CalendarView's display options). Pair with useEscapeClose for Escape.
//
// - On open: focus the first focusable control in the panel (roving-tabindex
//   items with tabindex="-1" are skipped, so a radio group lands on its
//   checked item).
// - On close: if focus was inside the wrapper (Escape, or an item that closes
//   the panel), hand it back to the trigger. A close caused by clicking
//   elsewhere leaves focus where the user put it.
import { nextTick, watch, type Ref } from 'vue'

const FOCUSABLE = [
  'button:not([disabled]):not([tabindex="-1"])',
  'input:not([disabled]):not([tabindex="-1"])',
  '[tabindex="0"]',
].join(',')

export function usePopoverFocus(
  open: Ref<boolean>,
  wrap: Ref<HTMLElement | null>,
  panel: Ref<HTMLElement | null>,
  /** Selector of the trigger inside `wrap`. */
  triggerSelector = '[aria-haspopup]',
) {
  watch(open, async (isOpen) => {
    if (isOpen) {
      await nextTick()
      panel.value?.querySelector<HTMLElement>(FOCUSABLE)?.focus()
      return
    }
    // Runs before the panel unmounts, so focus may still be inside it.
    const active = document.activeElement
    if (active && wrap.value?.contains(active)) {
      wrap.value.querySelector<HTMLElement>(triggerSelector)?.focus()
    }
  })
}
