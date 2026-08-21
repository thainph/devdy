// Manages the standalone "desktop pet" window for the DY Cyber Fox: a frameless,
// transparent, always-on-top OS window that floats the mascot on the desktop
// (outside the main app window). It loads the same SPA at
// `index.html?mascotWindow=1`; App.vue detects the flag and renders MascotWindow.
//
// State sync: the main window is the source of truth. CyberFoxHost computes the
// mascot phase from the live-runs store and pushes it here via emitMascotState;
// the mascot window listens for `mascot:state`. When the mascot window mounts it
// emits `mascot:ready` so the host can replay the current state immediately.
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import { LogicalSize } from '@tauri-apps/api/dpi'
import { emit } from '@tauri-apps/api/event'
import type { CyberFoxSize, CyberFoxState } from '@/composables/useMascotState'
import type { MascotBubbleMessage } from '@/composables/useMascotBubble'

export const MASCOT_WINDOW_LABEL = 'mascot'
export const MASCOT_STATE_EVENT = 'mascot:state'
export const MASCOT_READY_EVENT = 'mascot:ready'
export const MASCOT_BUBBLE_EVENT = 'mascot:bubble'
export const MASCOT_SPEAKING_EVENT = 'mascot:speaking'
export const MASCOT_QUICK_CREATE_EVENT = 'mascot:quick-create'

export interface MascotStatePayload {
  state: CyberFoxState
  size: CyberFoxSize
  streams: number
  lite?: boolean
  evolutionRealm?: string
  evolutionTier?: number
  /** Ascension stars ⭐ — completed 35-level cycles. */
  stars?: number
  levelUpAt?: number
}

// Vertical room reserved ABOVE the fox for the speech bubble (logical px).
const BUBBLE_ZONE = 110

/** Window footprint (logical px): a square fox area plus a bubble zone on top. */
export function mascotWindowSize(size: CyberFoxSize): { width: number; height: number } {
  const edge = size === 'sm' ? 240 : size === 'lg' ? 380 : 300
  return { width: edge, height: edge + BUBBLE_ZONE }
}

/** Open (or focus) the desktop-pet window and match its size to the preference. */
export async function openMascotWindow(size: CyberFoxSize = 'md'): Promise<WebviewWindow> {
  const { width, height } = mascotWindowSize(size)
  const existing = await WebviewWindow.getByLabel(MASCOT_WINDOW_LABEL)
  if (existing) {
    try {
      await existing.setSize(new LogicalSize(width, height))
      await existing.show()
    } catch {
      /* window may be mid-teardown */
    }
    return existing
  }

  const win = new WebviewWindow(MASCOT_WINDOW_LABEL, {
    // Slim standalone entry (mascot.html → src/mascot/main.ts) instead of booting
    // the full SPA (index.html?mascotWindow=1) inside a second webview.
    url: 'mascot.html',
    title: 'DY',
    width,
    height,
    resizable: false,
    decorations: false,
    transparent: true,
    alwaysOnTop: true,
    skipTaskbar: true,
    shadow: false,
    focus: false,
  })
  win.once('tauri://error', (e) => {
    console.error('[mascotWindow] failed to open window', e)
  })
  return win
}

/** Close the desktop-pet window if it is open (no-op otherwise). */
export async function closeMascotWindow(): Promise<void> {
  const existing = await WebviewWindow.getByLabel(MASCOT_WINDOW_LABEL)
  if (!existing) return
  try {
    await existing.close()
  } catch {
    /* already gone */
  }
}

/** Push the current mascot phase/size/streams to the desktop-pet window. */
export async function emitMascotState(payload: MascotStatePayload): Promise<void> {
  try {
    await emit(MASCOT_STATE_EVENT, payload)
  } catch {
    /* main window not in a Tauri shell (dev in browser) */
  }
}

/** Push a speech-bubble message to the desktop-pet window. */
export async function emitMascotBubble(payload: MascotBubbleMessage): Promise<void> {
  try {
    await emit(MASCOT_BUBBLE_EVENT, payload)
  } catch {
    /* main window not in a Tauri shell (dev in browser) */
  }
}

/** Push the "is talking" flag to the desktop-pet window (audio plays in main). */
export async function emitMascotSpeaking(speaking: boolean): Promise<void> {
  try {
    await emit(MASCOT_SPEAKING_EVENT, speaking)
  } catch {
    /* main window not in a Tauri shell (dev in browser) */
  }
}
