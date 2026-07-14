// App-wide toast notifications. A single <ToastHost> (mounted in App.vue)
// renders the shared queue; any component triggers one via `useToast()`:
//
//   import { useToast } from '@/composables/useToast'
//   const { toast } = useToast()
//   toast.success('Saved')
//   toast.error(String(e))
//
// Mirrors the useConfirm pattern (shared reactive state + one host) so toast
// styling/behaviour lives in exactly one place and stays consistent everywhere.
import { reactive } from 'vue'

export type ToastVariant = 'success' | 'error' | 'info'

export interface ToastItem {
  id: number
  message: string
  variant: ToastVariant
}

const state = reactive<{ items: ToastItem[] }>({ items: [] })
const timers = new Map<number, ReturnType<typeof setTimeout>>()
let seq = 0

function dismiss(id: number) {
  const i = state.items.findIndex((t) => t.id === id)
  if (i >= 0) state.items.splice(i, 1)
  const timer = timers.get(id)
  if (timer) {
    clearTimeout(timer)
    timers.delete(id)
  }
}

function show(message: string, variant: ToastVariant, duration: number): number {
  const id = ++seq
  state.items.push({ id, message, variant })
  if (duration > 0) {
    timers.set(id, setTimeout(() => dismiss(id), duration))
  }
  return id
}

// Errors linger a little longer than success/info so they're readable.
const toast = {
  show: (message: string, variant: ToastVariant = 'info', duration = 2500) =>
    show(message, variant, duration),
  success: (message: string, duration = 2500) => show(message, 'success', duration),
  error: (message: string, duration = 4000) => show(message, 'error', duration),
  info: (message: string, duration = 2500) => show(message, 'info', duration),
}

export function useToast() {
  return { state, toast, dismiss }
}
