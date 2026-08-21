// Pairs each Cyber Fox voice clip with the exact words it speaks, so the speech
// bubble shows what the fox is actually saying. `clip` is the file STEM in
// src/assets/mascot-sounds/ (no extension); `text` is the transcript shown in
// the bubble when that clip is picked.
//
// ⚠️ EDIT THE `text` VALUES to match your recordings exactly. The lines below
// are best-guess transcripts based on the suggested phrases — adjust them so
// each string is verbatim what its audio file says. Add/remove entries here as
// you add/remove clips. A clip listed here but missing on disk is skipped; a
// clip on disk but not listed here still plays (as a variant fallback) but
// shows the generic bubble text instead of a matched line.
import type { MascotBubbleVariant } from '@/composables/useMascotBubble'

export interface MascotVoiceLine {
  /** File stem in src/assets/mascot-sounds/ (without extension). */
  clip: string
  /** Verbatim words spoken in the clip — shown in the bubble. */
  text: string
}

/** Variants that can carry a voice line, plus the "default" safety-net group. */
export type MascotVoiceKey = MascotBubbleVariant | 'default'

export const MASCOT_VOICE_LINES: Partial<Record<MascotVoiceKey, MascotVoiceLine[]>> = {
  thinking: [
    { clip: 'thinking-1', text: "Give me a sec — I'm working on it right now." },
    { clip: 'thinking-2', text: 'Hmm, crunching through this… hang tight!' },
    { clip: 'thinking-3', text: 'On it! Let me figure this out for you.' },
    { clip: 'thinking-4', text: 'Alright, let me dig into this for you…' },
  ],
  permission: [
    { clip: 'permission-1', text: 'Hey, I need your go-ahead before I continue.' },
    { clip: 'permission-2', text: 'Quick check — can I go ahead and do this?' },
    { clip: 'permission-3', text: "Hold on, I need your permission for this one." },
  ],
  success: [
    { clip: 'success-1', text: 'All done! Everything went through nicely.' },
    { clip: 'success-2', text: 'Yay, all wrapped up — take a look!' },
  ],
  error: [
    { clip: 'error-1', text: "Uh-oh, that didn't go as planned." },
    { clip: 'error-2', text: 'Oops… something went wrong there.' },
  ],
  info: [
    { clip: 'info-1', text: "Heads up — here's something you should know." },
  ],
  // Safety net: used only when a variant above has no take of its own. Edit the
  // text to match your default.mp3 recording.
  default: [
    { clip: 'default', text: "Hey there! Here's an update for you." },
  ],
}
