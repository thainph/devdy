// Promise-based text-input dialog, sibling to useConfirm. A single <PromptModal>
// host (mounted in App.vue) reads this shared state and resolves the pending
// promise with the entered string, or null when cancelled:
//
//   import { usePrompt } from '@/composables/usePrompt'
//   const { prompt } = usePrompt()
//   const name = await prompt({ title: 'File mới', label: 'Tên file' })
//   if (!name) return
//
import { reactive } from 'vue'

export interface PromptOptions {
  /** Header text. */
  title?: string
  /** Small label rendered above the input. */
  label?: string
  /** Pre-filled value (e.g. current name when renaming). */
  initialValue?: string
  /** Input placeholder. */
  placeholder?: string
  /** Confirm-button label. */
  confirmLabel?: string
  /** Cancel-button label. */
  cancelLabel?: string
  /** Select the initial value on open (handy for rename). */
  selectAll?: boolean
}

interface PromptState extends Required<PromptOptions> {
  open: boolean
  value: string
}

const state = reactive<PromptState>({
  open: false,
  title: 'Enter a value',
  label: '',
  initialValue: '',
  placeholder: '',
  confirmLabel: 'OK',
  cancelLabel: 'Cancel',
  selectAll: false,
  value: '',
})

let resolver: ((value: string | null) => void) | null = null

function prompt(options: PromptOptions | string): Promise<string | null> {
  const opts = typeof options === 'string' ? { title: options } : options

  // If a dialog is already pending, cancel it before opening the new one.
  resolver?.(null)

  state.title = opts.title ?? 'Enter a value'
  state.label = opts.label ?? ''
  state.initialValue = opts.initialValue ?? ''
  state.placeholder = opts.placeholder ?? ''
  state.confirmLabel = opts.confirmLabel ?? 'OK'
  state.cancelLabel = opts.cancelLabel ?? 'Cancel'
  state.selectAll = opts.selectAll ?? false
  state.value = opts.initialValue ?? ''
  state.open = true

  return new Promise<string | null>((resolve) => {
    resolver = resolve
  })
}

function respond(value: string | null) {
  if (!state.open) return
  state.open = false
  resolver?.(value)
  resolver = null
}

export function usePrompt() {
  return { state, prompt, respond }
}
