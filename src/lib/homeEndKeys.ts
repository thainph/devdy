// macOS WebKit maps Home/End to "scroll the document" instead of moving the
// caret, so in a text field they appear to do nothing. Give them the
// Windows/Linux meaning: Home/End jump to the start/end of the current line,
// Ctrl/Cmd+Home/End to the start/end of the whole text, Shift extends the
// selection.

type TextField = HTMLTextAreaElement | HTMLInputElement

const TEXT_INPUT_TYPES = new Set(['text', 'search', 'url', 'email', 'tel', 'password', ''])

function textField(el: EventTarget | null): TextField | null {
  if (el instanceof HTMLTextAreaElement) return el
  if (el instanceof HTMLInputElement && TEXT_INPUT_TYPES.has(el.type)) return el
  return null
}

function onKeydown(e: KeyboardEvent) {
  if (e.key !== 'Home' && e.key !== 'End') return
  if (e.defaultPrevented || e.isComposing || e.altKey) return
  const el = textField(e.target)
  if (!el || el.readOnly || el.disabled) return

  const toEnd = e.key === 'End'
  const wholeText = e.metaKey || e.ctrlKey || el instanceof HTMLInputElement
  e.preventDefault()

  const start = el.selectionStart ?? 0
  const end = el.selectionEnd ?? 0
  const backward = el.selectionDirection === 'backward'
  // The end of the selection that moves; the other one stays put on Shift.
  const focus = backward ? start : end
  const anchor = backward ? end : start

  let target: number
  if (wholeText) {
    target = toEnd ? el.value.length : 0
  } else {
    // Prefer the visual (wrapped) line, as the platform shortcuts would.
    const sel = window.getSelection()
    if (sel?.modify) {
      el.setSelectionRange(focus, focus)
      sel.modify('move', toEnd ? 'forward' : 'backward', 'lineboundary')
      target = el.selectionStart ?? focus
    } else {
      const v = el.value
      target = toEnd
        ? (v.indexOf('\n', focus) === -1 ? v.length : v.indexOf('\n', focus))
        : v.lastIndexOf('\n', focus - 1) + 1
    }
  }

  if (e.shiftKey) {
    if (target < anchor) el.setSelectionRange(target, anchor, 'backward')
    else el.setSelectionRange(anchor, target, 'forward')
  } else {
    el.setSelectionRange(target, target)
  }
}

export function installHomeEndKeys() {
  if (!/mac/i.test(navigator.userAgent)) return
  // Bubble phase so component handlers (pickers, palettes) can claim the key
  // first via preventDefault.
  window.addEventListener('keydown', onKeydown)
}
