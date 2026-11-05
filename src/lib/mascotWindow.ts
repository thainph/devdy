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
const BUBBLE_ZONE = 96

/**
 * Window footprint (logical px). Kept SNUG around the actual fox sprite (which is
 * 96 / 148 / 196 px, see CyberFoxCanvas SIZES) plus a small glow margin and the
 * speech-bubble zone on top. A tight window matters on macOS: the old oversized
 * frame carried ~150px of invisible margin above the fox, so macOS (which won't
 * let a window drag above the menu bar) blocked the fox at mid-screen and the
 * empty margin overlapped the menu bar where the transparent canvas goes blank.
 */
export function mascotWindowSize(size: CyberFoxSize): { width: number; height: number } {
  const fox = size === 'sm' ? 96 : size === 'lg' ? 196 : 148
  const box = fox + 44 // room for the ground-ring glow / drop shadows
  return { width: box, height: box + BUBBLE_ZONE }
}

// In-flight creation guard: several watchers (enabled/mode, size, ready-replay)
// can call openMascotWindow near-simultaneously on startup. Without this, each
// passes the getByLabel() null check before any has finished creating, spawning
// duplicate windows (the extra one gets stranded in the screen centre).
let creating: Promise<WebviewWindow> | null = null

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
  // A creation kicked off by another caller is already running — reuse it.
  if (creating) return creating

  creating = Promise.resolve(createMascotWindow(width, height))
  try {
    return await creating
  } finally {
    creating = null
  }
}

function createMascotWindow(width: number, height: number): WebviewWindow {
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
    // Start hidden: Tauri would otherwise place a positionless window in the
    // CENTRE of the screen, so the fox flashes there for a frame before
    // MascotWindow.onMounted moves it to its saved/bottom-right spot. The pet
    // stays invisible until that reposition is done, then calls show().
    visible: false,
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
