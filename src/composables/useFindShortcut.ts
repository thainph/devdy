// ⌘F / Ctrl+F for a screen's search box: runs `onFind` only while the box's
// screen is the one in view — the element is rendered and visible, and while
// split it sits in the focused half (the half that last took a click, like
// ⌘B in composables/useListPane). With two boxes in view (a screen and its
// floating list…), the one holding focus wins, else the first. The element
// must carry the `data-find-shortcut` attribute.
//
// `autoFocus` also puts the cursor in the box whenever its screen comes into
// view (mounted, or re-shown from a KeepAlive tab), so typing searches right
// away — unless the user is already typing somewhere or a dialog is open.
import { nextTick, onActivated, onBeforeUnmount, onMounted, type Ref } from 'vue'
import { useWorkspaceTabsStore } from '@/stores/workspaceTabs'

const ATTR = 'data-find-shortcut'

function visible(el: Element): boolean {
  return el.isConnected && el.getClientRects().length > 0
}

/** Focus is in a field (or an editor) the user may be typing in. */
function typing(): boolean {
  const a = document.activeElement
  if (!(a instanceof HTMLElement) || a === document.body) return false
  return a.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(a.tagName)
}

function dialogOpen(): boolean {
  return [...document.querySelectorAll('[role="dialog"], [role="alertdialog"]')].some(visible)
}

export interface FindShortcutOptions {
  /**
   * Focus the box when its screen comes into view: `true` runs `onFind`, a
   * function runs instead (e.g. focus without opening an overlay).
   */
  autoFocus?: boolean | (() => void)
}

export function useFindShortcut(el: Ref<HTMLElement | null>, onFind: () => void, opts: FindShortcutOptions = {}) {
  const tabs = useWorkspaceTabsStore()

  function owns(): boolean {
    const me = el.value
    if (!me || !visible(me)) return false
    const myPane = me.closest('[data-pane]')
    if (tabs.isSplit && myPane && myPane.getAttribute('data-pane') !== tabs.focusedPane) return false
    const shown = [...document.querySelectorAll(`[${ATTR}]`)].filter(visible)
    if (shown.length <= 1) return true
    const inPane = myPane ? shown.filter((x) => myPane.contains(x)) : shown
    const focused = inPane.find((x) => x.contains(document.activeElement))
    return (focused ?? inPane[0]) === me
  }

  function onKeydown(e: KeyboardEvent) {
    if (!(e.metaKey || e.ctrlKey) || e.altKey || e.shiftKey || e.key.toLowerCase() !== 'f') return
    if (e.defaultPrevented || !owns()) return
    e.preventDefault()
    onFind()
  }

  // After the screen has laid out (a re-shown tab is still display:none on activation).
  function focusSoon() {
    const run = opts.autoFocus === true ? onFind : opts.autoFocus
    if (!run) return
    void nextTick(() => requestAnimationFrame(() => {
      if (owns() && !typing() && !dialogOpen()) run()
    }))
  }

  onMounted(() => {
    window.addEventListener('keydown', onKeydown)
    focusSoon()
  })
  // Fires only under KeepAlive (cached views, surface tabs); the first one
  // coincides with mount, where focusing twice is harmless.
  onActivated(focusSoon)
  onBeforeUnmount(() => window.removeEventListener('keydown', onKeydown))
}

/**
 * A screen's plain search field: `el` wraps the `<input>` and carries
 * `data-find-shortcut`. ⌘F and entering the screen focus it.
 */
export function useSearchBox(el: Ref<HTMLElement | null>) {
  useFindShortcut(el, () => {
    const input = el.value?.querySelector('input')
    input?.focus()
    input?.select()
  }, { autoFocus: true })
}
