// Shared "speech bubble" channel for the DY Cyber Fox. A single reactive slot
// holds the message the fox is currently "saying"; any part of the app can push
// to it (mirrors the useToast pattern). The bubble is rendered by MascotBubble
// next to the fox in BOTH surfaces (in-app + desktop pet). The desktop pet gets
// its copy forwarded over a Tauri event by CyberFoxHost.
import { reactive } from 'vue'

export type MascotBubbleVariant = 'info' | 'success' | 'error' | 'thinking' | 'permission'

export interface MascotBubbleMessage {
  id: number
  text: string
  variant: MascotBubbleVariant
  /** Auto-hide delay in ms (the display component owns the timer). */
  duration: number
}

// Sensible default lifetimes per kind: errors/permission linger, chatter is brief.
const DEFAULT_DURATION: Record<MascotBubbleVariant, number> = {
  info: 4500,
  success: 3500,
  error: 6000,
  thinking: 4000,
  permission: 8000,
}

const state = reactive<{ current: MascotBubbleMessage | null }>({ current: null })
let seq = 0

/** Show a message in the fox's speech bubble. Returns the message id. */
function push(
  text: string,
  variant: MascotBubbleVariant = 'info',
  duration?: number,
): number {
  const id = ++seq
  state.current = {
    id,
    text,
    variant,
    duration: duration ?? DEFAULT_DURATION[variant],
  }
  return id
}

/** Clear the current bubble (e.g. user dismissed it). */
function clear() {
  state.current = null
}

export function useMascotBubble() {
  return { state, push, clear }
}
