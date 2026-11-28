// Keyboard navigation of an inbox list (PR Inbox, GitHub Mentions). Bind
// `onKeydown` on the focusable list container; rows carry
// `data-inbox-key="<key>"` so the selection scrolls into view. Keys: see
// lib/inboxKeys.
import { nextTick, ref } from 'vue'
import { inboxKeyAction, moveSelection } from '@/lib/inboxKeys'

export function useInboxKeyboard(opts: {
  /** Row keys in visible order (collapsed groups excluded). */
  keys: () => string[]
  root: () => HTMLElement | null
  onPrimary: (key: string) => void
  onOpen: (key: string) => void
  onCopy: (key: string) => void
}) {
  const selected = ref<string | null>(null)

  function select(key: string) {
    selected.value = key
  }

  function onKeydown(e: KeyboardEvent) {
    const target = e.target as HTMLElement | null
    if (target?.closest('input, textarea, select, [contenteditable="true"]')) return
    const action = inboxKeyAction(e.key, { meta: e.metaKey, ctrl: e.ctrlKey, alt: e.altKey })
    if (!action) return
    // Enter on a focused button is that button's own click.
    if (action === 'primary' && target?.closest('button, a')) return
    const keys = opts.keys()
    if (action === 'next' || action === 'prev') {
      e.preventDefault()
      selected.value = moveSelection(keys, selected.value, action === 'next' ? 1 : -1)
      const key = selected.value
      if (key) {
        void nextTick(() => {
          opts.root()?.querySelector(`[data-inbox-key="${CSS.escape(key)}"]`)?.scrollIntoView({ block: 'nearest' })
        })
      }
      return
    }
    const key = selected.value
    if (!key || !keys.includes(key)) return
    e.preventDefault()
    if (action === 'primary') opts.onPrimary(key)
    else if (action === 'open') opts.onOpen(key)
    else opts.onCopy(key)
  }

  return { selected, select, onKeydown }
}
