// Browser text-to-speech for the DY Cyber Fox. Used for languages that ship no
// recorded voice clips (currently Vietnamese): instead of playing an mp3, the
// fox "speaks" the bubble text through the Web Speech API in the matching
// language. It mirrors useMascotSound's contract — it drives the same
// begin/endSpeaking mouth-flap signal and never talks over itself — so callers
// can swap between recorded audio and TTS transparently.
import { onBeforeUnmount, ref, type Ref } from 'vue'
import { beginSpeaking, endSpeaking } from '@/composables/useMascotSpeaking'

// App locale → BCP-47 tag we ask the synthesizer for.
const LANG_TAG: Record<string, string> = {
  vi: 'vi-VN',
  en: 'en-US',
}

// TTS reads the whole bubble text aloud; skip anything unreasonably long so the
// fox never launches into a monologue over a big toast/error dump.
const MAX_SPEAK_CHARS = 220

/** Per-utterance overrides (chosen voice + rate/pitch from settings). */
export interface MascotSpeechOptions {
  /** Preferred voice NAME (SpeechSynthesisVoice.name). Empty → auto by locale. */
  voiceName?: string
  /** Speaking rate (~0.5–2.0). */
  rate?: number
  /** Voice pitch (~0–2.0). */
  pitch?: number
}

function synth(): SpeechSynthesis | null {
  if (typeof window === 'undefined' || !('speechSynthesis' in window)) return null
  return window.speechSynthesis
}

// Voices can load asynchronously; warm them up once so getVoices() is populated
// by the time the fox first speaks. Use addEventListener so we don't clobber any
// other voiceschanged handler.
if (typeof window !== 'undefined' && 'speechSynthesis' in window) {
  try {
    window.speechSynthesis.getVoices()
    window.speechSynthesis.addEventListener('voiceschanged', () =>
      window.speechSynthesis.getVoices(),
    )
  } catch {
    /* not supported — speak() will no-op */
  }
}

function clamp(n: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, n))
}

/** All installed voices for a locale (matched on the BCP-47 language prefix). */
export function listVoices(locale: string): SpeechSynthesisVoice[] {
  const s = synth()
  if (!s) return []
  const short = locale.toLowerCase()
  return s.getVoices().filter((v) => v.lang?.toLowerCase().startsWith(short))
}

/**
 * Best voice for this utterance: the explicitly chosen one (by name) if it is
 * installed, otherwise the first voice matching the locale, otherwise null (let
 * the engine use its default).
 */
function pickVoice(locale: string, voiceName?: string): SpeechSynthesisVoice | null {
  const s = synth()
  if (!s) return null
  const voices = s.getVoices()
  if (!voices.length) return null
  if (voiceName) {
    const chosen = voices.find((v) => v.name === voiceName)
    if (chosen) return chosen
  }
  const tag = (LANG_TAG[locale] ?? locale).toLowerCase()
  const short = locale.toLowerCase()
  return (
    voices.find((v) => v.lang?.toLowerCase() === tag) ??
    voices.find((v) => v.lang?.toLowerCase().startsWith(short)) ??
    null
  )
}

/** Is the browser able to speak (Web Speech API present)? */
export function speechSupported(): boolean {
  return synth() !== null
}

/**
 * Reactive list of installed voices for a locale, kept in sync as the browser
 * finishes loading them (voices often arrive after the first paint). Use in the
 * Settings voice picker. Auto-unregisters its listener on unmount.
 */
export function useVoiceList(locale: string): Ref<SpeechSynthesisVoice[]> {
  const voices = ref<SpeechSynthesisVoice[]>(listVoices(locale))
  const s = synth()
  const refresh = () => {
    voices.value = listVoices(locale)
  }
  if (s) {
    s.addEventListener('voiceschanged', refresh)
    onBeforeUnmount(() => s.removeEventListener('voiceschanged', refresh))
  }
  return voices
}

/**
 * Shared mascot TTS player. Best-effort: returns false (and stays silent) when
 * speech synthesis is unavailable, the text is empty/too long, or the fox is
 * already speaking — so voices never overlap, matching the audio player.
 */
export function useMascotSpeech() {
  function speak(text: string, locale = 'en', opts: MascotSpeechOptions = {}): boolean {
    const s = synth()
    if (!s) return false
    const words = text?.trim()
    if (!words || words.length > MAX_SPEAK_CHARS) return false
    // Already talking (audio clip or a previous utterance) → drop this one.
    if (s.speaking || s.pending) return false

    const utter = new SpeechSynthesisUtterance(words)
    utter.lang = LANG_TAG[locale] ?? locale
    const voice = pickVoice(locale, opts.voiceName)
    if (voice) utter.voice = voice
    utter.rate = clamp(Number.isFinite(opts.rate) ? (opts.rate as number) : 1, 0.5, 2)
    utter.pitch = clamp(Number.isFinite(opts.pitch) ? (opts.pitch as number) : 1.15, 0, 2)
    utter.volume = 0.9
    utter.onstart = () => beginSpeaking()
    utter.onend = () => endSpeaking()
    utter.onerror = () => endSpeaking()
    try {
      s.speak(utter)
    } catch {
      endSpeaking()
      return false
    }
    return true
  }

  /** Stop any in-progress speech (e.g. mascot disabled mid-sentence). */
  function stop() {
    const s = synth()
    if (s) s.cancel()
    endSpeaking()
  }

  return { speak, stop }
}
