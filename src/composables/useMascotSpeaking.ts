// Shared reactive "is the mascot speaking right now?" signal. Set by the mascot
// sound player (useMascotSound) around actual audio playback, and read by
// CyberFox to drive the talking mouth/jaw animation. Module-scoped so every
// caller in the SAME window shares one source of truth (the desktop-pet window
// is a separate process — CyberFoxHost forwards this flag to it over an event).
import { readonly, ref } from 'vue'

const speaking = ref(false)
let safetyTimer: ReturnType<typeof setTimeout> | null = null

function clearSafety() {
  if (safetyTimer) {
    clearTimeout(safetyTimer)
    safetyTimer = null
  }
}

/**
 * Mark the mascot as speaking. `maxMs` is a safety net: if `endSpeaking()` never
 * fires (e.g. an audio "ended" event is missed), the flag auto-clears so the
 * mouth doesn't flap forever.
 */
export function beginSpeaking(maxMs = 8000) {
  speaking.value = true
  clearSafety()
  if (maxMs > 0) {
    safetyTimer = setTimeout(() => {
      speaking.value = false
      safetyTimer = null
    }, maxMs)
  }
}

export function endSpeaking() {
  clearSafety()
  speaking.value = false
}

/** Directly set the flag (used by windows that receive it over an event). */
export function setSpeaking(value: boolean) {
  if (value) beginSpeaking()
  else endSpeaking()
}

// Read-only ref for components that just want to observe the state.
export const mascotSpeaking = readonly(speaking)

export function useMascotSpeaking() {
  return { speaking: mascotSpeaking, beginSpeaking, endSpeaking, setSpeaking }
}
