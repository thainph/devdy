// ⌘F / Ctrl+F for a screen's search box: runs `onFind` only while the box's
// screen is the one in view — the element is rendered and visible, and while
// split it sits in the focused half (the half that last took a click, like
// ⌘B in composables/useListPane). With two boxes in view (a screen and its
// floating list…), the one holding focus wins, else the first. The element
// must carry the `data-find-shortcut` attribute.
import { onBeforeUnmount, onMounted, type Ref } from 'vue'
import { useWorkspaceTabsStore } from '@/stores/workspaceTabs'

const ATTR = 'data-find-shortcut'

function visible(el: Element): boolean {
  return el.isConnected && el.getClientRects().length > 0
}

export function useFindShortcut(el: Ref<HTMLElement | null>, onFind: () => void) {
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

  onMounted(() => window.addEventListener('keydown', onKeydown))
  onBeforeUnmount(() => window.removeEventListener('keydown', onKeydown))
}
