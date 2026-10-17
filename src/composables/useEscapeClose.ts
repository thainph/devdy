// Close a popover on Escape WITHOUT also closing the dialog it sits in.
//
// Modal (and Drawer) close on a window `keydown` Escape listener, and radix-vue
// popovers listen on window too — so a single Escape would dismiss both. While
// `open`, this listens in the CAPTURE phase (ahead of every bubble listener),
// closes the popover itself and stops the event there.
import { onBeforeUnmount, watch, type Ref } from 'vue'

export function useEscapeClose(open: Ref<boolean>) {
  function onKey(e: KeyboardEvent) {
    if (e.key !== 'Escape') return
    e.stopPropagation()
    open.value = false
  }
  watch(open, (v) => {
    if (v) window.addEventListener('keydown', onKey, true)
    else window.removeEventListener('keydown', onKey, true)
  })
  onBeforeUnmount(() => window.removeEventListener('keydown', onKey, true))
}
