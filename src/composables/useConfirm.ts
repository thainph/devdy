// Promise-based confirm dialog to replace the browser-native `confirm()`.
// A single <ConfirmModal> host (mounted in App.vue) reads this shared state and
// resolves the pending promise. Callers await the boolean, exactly like
// `confirm()`:
//
//   import { useConfirm } from '@/composables/useConfirm'
//   const { confirm } = useConfirm()
//   if (!(await confirm('Delete this?'))) return
//
import { reactive } from 'vue'

export interface ConfirmOptions {
  /** Header text. */
  title?: string
  /** Body text; `\n` renders as line breaks. */
  message: string
  /** Confirm-button label. */
  confirmLabel?: string
  /** Cancel-button label. */
  cancelLabel?: string
  /** Confirm-button style. `destructive` (default) is red for delete actions. */
  variant?: 'destructive' | 'primary'
}

interface ConfirmState extends Required<ConfirmOptions> {
  open: boolean
}

const state = reactive<ConfirmState>({
  open: false,
  title: 'Confirm',
  message: '',
  confirmLabel: 'Confirm',
  cancelLabel: 'Cancel',
  variant: 'destructive',
})

let resolver: ((value: boolean) => void) | null = null

function confirm(options: ConfirmOptions | string): Promise<boolean> {
  const opts = typeof options === 'string' ? { message: options } : options

  // If a dialog is already pending, cancel it before opening the new one.
  resolver?.(false)

  state.title = opts.title ?? 'Confirm'
  state.message = opts.message
  state.confirmLabel = opts.confirmLabel ?? 'Confirm'
  state.cancelLabel = opts.cancelLabel ?? 'Cancel'
  state.variant = opts.variant ?? 'destructive'
  state.open = true

  return new Promise<boolean>((resolve) => {
    resolver = resolve
  })
}

function respond(value: boolean) {
  if (!state.open) return
  state.open = false
  resolver?.(value)
  resolver = null
}

export function useConfirm() {
  return { state, confirm, respond }
}
