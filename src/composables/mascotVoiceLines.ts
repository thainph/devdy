// Pairs each Cyber Fox voice line with the exact words it speaks, per language,
// so the speech bubble shows what the fox is actually saying.
//
// English lines carry a `clip` — the file STEM in src/assets/mascot-sounds/
// (no extension) — and are played back as recorded audio. Languages with no
// recordings (Vietnamese) omit `clip`; those lines are spoken with the browser's
// TTS voice instead (see useMascotSpeech), and the bubble text IS what gets read
// aloud. The active language follows the app locale.
//
// A clip listed but missing on disk is skipped; a clipless line is always
// eligible (TTS handles it). Add/remove entries here as you add/remove takes.
import type { MascotBubbleVariant } from '@/composables/useMascotBubble'

export interface MascotVoiceLine {
  /**
   * File stem in src/assets/mascot-sounds/ (without extension). Omit for
   * languages with no recorded clips — those lines are spoken via browser TTS.
   */
  clip?: string
  /** Verbatim words spoken in the clip / read aloud — shown in the bubble. */
  text: string
}

/** Variants that can carry a voice line, plus the "default" safety-net group. */
export type MascotVoiceKey = MascotBubbleVariant | 'default'

export type MascotVoiceLines = Partial<Record<MascotVoiceKey, MascotVoiceLine[]>>

/**
 * Voice lines per app locale. Falls back to `en` for any locale not listed here
 * (see pickMascotVoice in useMascotSound).
 */
export const MASCOT_VOICE_LINES: Record<string, MascotVoiceLines> = {
  en: {
    thinking: [
      { clip: 'thinking-1', text: "Give me a sec — I'm working on it right now." },
      { clip: 'thinking-2', text: 'Hmm, crunching through this… hang tight!' },
      { clip: 'thinking-3', text: 'On it! Let me figure this out for you.' },
      { clip: 'thinking-4', text: 'Alright, let me dig into this for you…' },
    ],
    permission: [
      { clip: 'permission-1', text: 'Hey, I need your go-ahead before I continue.' },
      { clip: 'permission-2', text: 'Quick check — can I go ahead and do this?' },
      { clip: 'permission-3', text: 'Hold on, I need your permission for this one.' },
    ],
    success: [
      { clip: 'success-1', text: 'All done! Everything went through nicely.' },
      { clip: 'success-2', text: 'Yay, all wrapped up — take a look!' },
    ],
    error: [
      { clip: 'error-1', text: "Uh-oh, that didn't go as planned." },
      { clip: 'error-2', text: 'Oops… something went wrong there.' },
    ],
    info: [{ clip: 'info-1', text: "Heads up — here's something you should know." }],
    // Safety net: used only when a variant above has no take of its own.
    default: [{ clip: 'default', text: "Hey there! Here's an update for you." }],
  },
  // Vietnamese — spoken via browser TTS (no recorded clips). The bubble text is
  // what gets read aloud, so keep it natural and spoken-friendly.
  vi: {
    thinking: [
      { text: 'Chờ mình một chút — đang xử lý ngay đây.' },
      { text: 'Hmm, để mình tính toán cái này… ráng đợi tí nhé!' },
      { text: 'Đang làm liền! Để mình lo cho bạn.' },
      { text: 'Được rồi, để mình đào sâu cái này nào…' },
    ],
    permission: [
      { text: 'Nè, mình cần bạn đồng ý trước khi làm tiếp.' },
      { text: 'Hỏi nhanh — mình tiến hành cái này được chứ?' },
      { text: 'Khoan đã, mình cần bạn cấp quyền cho việc này.' },
    ],
    success: [
      { text: 'Xong hết rồi! Mọi thứ chạy ngon lành.' },
      { text: 'Tuyệt, hoàn tất rồi — bạn xem thử nhé!' },
    ],
    error: [
      { text: 'Ối, việc này không như dự tính rồi.' },
      { text: 'Rồi… có gì đó trục trặc mất tiêu.' },
    ],
    info: [{ text: 'Nhắc nhẹ — có chuyện này bạn nên biết nè.' }],
    default: [{ text: 'Chào bạn! Mình có cập nhật cho bạn đây.' }],
  },
}
