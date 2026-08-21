// Plays a short DY Cyber Fox "voice" sound whenever a speech bubble appears, and
// pairs each clip with the exact words it speaks (see mascotVoiceLines.ts) so the
// bubble can show what the fox is actually saying.
//
// Clips live in src/assets/mascot-sounds/ and are loaded at build time via
// import.meta.glob. A file belongs to a variant when its name (without
// extension) is the variant itself OR starts with "<variant>-" / "<variant>_",
// so you can drop several takes per state:
//
//   success.mp3   success-1.mp3   success_2.wav   → success
//   thinking-1..4.mp3                             → thinking
//   default.mp3                                   → fallback
//
// If a variant has no clip it falls back to "default.*". If no files exist at
// all, play() is a silent no-op.
import type { MascotBubbleVariant } from '@/composables/useMascotBubble'
import { MASCOT_VOICE_LINES, type MascotVoiceLine } from '@/composables/mascotVoiceLines'
import { beginSpeaking, endSpeaking } from '@/composables/useMascotSpeaking'

const VARIANTS = ['info', 'success', 'error', 'thinking', 'permission', 'default'] as const

// Eagerly resolve every audio file's URL under the sounds folder.
const urlMap = import.meta.glob('../assets/mascot-sounds/*.{mp3,wav,ogg,m4a,webm,aac}', {
  eager: true,
  query: '?url',
  import: 'default',
}) as Record<string, string>

/** Filename without directory or extension, lower-cased. */
function stemOf(path: string): string {
  const file = path.split('/').pop() ?? ''
  return file.replace(/\.[^.]+$/, '').toLowerCase()
}

/** Which variant a file belongs to (exact stem, or "<variant>-"/"<variant>_" prefix). */
function variantOfStem(stem: string): string | null {
  for (const v of VARIANTS) {
    if (stem === v || stem.startsWith(`${v}-`) || stem.startsWith(`${v}_`)) return v
  }
  return null
}

// stem → resolved URL (each file has a unique stem).
const stemUrl: Record<string, string> = {}
// variant → all its clip URLs (for the random-fallback path).
const clips: Record<string, string[]> = {}
for (const [path, url] of Object.entries(urlMap)) {
  const stem = stemOf(path)
  stemUrl[stem] = url
  const v = variantOfStem(stem)
  if (v) (clips[v] ??= []).push(url)
}

function randomOf<T>(list: T[]): T {
  return list.length === 1 ? list[0] : list[Math.floor(Math.random() * list.length)]
}

/**
 * Pick a voice line (clip + matching text) for a variant, choosing at random
 * among the takes whose audio file actually exists on disk. Returns null when
 * the variant has no mapped-and-present line — callers then fall back to the
 * generic bubble text and a variant-random sound.
 */
export function pickMascotVoice(variant: MascotBubbleVariant): MascotVoiceLine | null {
  const present = (l: MascotVoiceLine) => Boolean(stemUrl[l.clip.toLowerCase()])
  const own = (MASCOT_VOICE_LINES[variant] ?? []).filter(present)
  if (own.length) return randomOf(own)
  // Fall back to the shared "default" take so the bubble still shows matching words.
  const fallback = (MASCOT_VOICE_LINES.default ?? []).filter(present)
  if (fallback.length) return randomOf(fallback)
  return null
}

// One reusable <audio> element per URL (rewound on each play).
const elements = new Map<string, HTMLAudioElement>()

function elementFor(url: string): HTMLAudioElement {
  let el = elements.get(url)
  if (!el) {
    el = new Audio(url)
    el.preload = 'auto'
    el.volume = 0.55
    elements.set(url, el)
  }
  return el
}

// The clip currently playing (module-scoped so it's shared across every caller).
// While one is playing we drop new requests, so the fox never talks over itself.
let activeAudio: HTMLAudioElement | null = null

function isBusy(): boolean {
  return !!activeAudio && !activeAudio.paused && !activeAudio.ended
}

/**
 * Shared mascot-sound player. Best-effort playback (autoplay policies may reject
 * the first play until the user interacts with the window; errors are swallowed).
 * Pass `clipStem` to play a specific take (kept in sync with the bubble text);
 * otherwise a clip is chosen at random for the variant.
 */
export function useMascotSound() {
  function play(variant: MascotBubbleVariant, clipStem?: string | null) {
    if (typeof window === 'undefined') return
    // A clip is still playing → skip this one so voices never overlap/duplicate.
    if (isBusy()) return
    let url: string | undefined = clipStem ? stemUrl[clipStem.toLowerCase()] : undefined
    if (!url) {
      const list = clips[variant]?.length ? clips[variant] : (clips.default ?? [])
      if (list.length) url = randomOf(list)
    }
    if (!url) return // nothing installed → stay silent
    const el = elementFor(url)
    const release = () => {
      if (activeAudio === el) activeAudio = null
      // Stop the talking animation as soon as the clip ends / is interrupted.
      endSpeaking()
    }
    // Drive the talking animation off the element's own playback events — this is
    // more reliable than the play() promise (which can resolve late or be
    // rejected by autoplay policy while audio still starts on a later gesture).
    const onPlaying = () => {
      const ms = Number.isFinite(el.duration) ? el.duration * 1000 + 400 : undefined
      beginSpeaking(ms)
    }
    try {
      el.currentTime = 0
      activeAudio = el
      el.onplay = onPlaying
      el.onplaying = onPlaying
      // Clear the guard when the clip finishes (or is stopped) so the next one can play.
      el.onended = release
      el.onpause = release
      el.onerror = release
      const p = el.play()
      if (p && typeof p.catch === 'function') p.catch(release)
    } catch {
      // autoplay blocked or element not ready — free the guard immediately.
      release()
    }
  }

  return { play }
}
