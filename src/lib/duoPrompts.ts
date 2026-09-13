/**
 * Prompt templates for the Duo Session feature (two AI sessions that take turns
 * talking to each other in a loop). Kept dependency-free so both the
 * orchestrator store and any tests can import it without pulling in Tauri/Pinia.
 *
 * The templates are GENERIC: the two sides are just "A" (goes first) and "B"
 * (responds), each with a caller-supplied role instruction and label. Nothing
 * here assumes a design/review use-case — that (or rock-paper-scissors, a
 * debate, etc.) comes entirely from the goal + role instructions the caller
 * passes in.
 *
 * The optional consensus token is ASYMMETRIC: only B (the responding/reviewing
 * side) may emit it, and only once it has nothing left to raise. A is told
 * explicitly that it cannot sign off on its own work, so a design turn can never
 * end the loop by quoting the token.
 */

/** Which side a prompt is addressed to: 'a' goes first, 'b' responds. */
export type DuoSide = 'a' | 'b'

export interface DuoPromptConfig {
  /** The shared task/topic that seeds the whole conversation. */
  goal: string
  /** Display names for the two sides (injected into prompts). */
  aLabel: string
  bLabel: string
  /** Role behaviour for each side (free text; may be empty). */
  aInstruction: string
  bInstruction: string
  /** Token B emits when the objective is met (empty = no consensus stop). */
  consensusToken: string
}

/** Join non-empty sections with blank lines between them. */
function joinSections(parts: string[]): string {
  return parts.map((p) => p.trim()).filter(Boolean).join('\n\n')
}

/**
 * The consensus instruction for the side being addressed, or '' when no token is
 * configured. Only B may end the loop; A gets a hard ban so it never signs off on
 * its own work.
 */
function consensusNote(cfg: DuoPromptConfig, side: DuoSide): string {
  if (!cfg.consensusToken) return ''
  if (side === 'a') {
    return [
      `Bạn KHÔNG có quyền chốt cuộc trao đổi. Chỉ **${cfg.bLabel}** mới được phép kết luận đã đạt yêu cầu.`,
      `TUYỆT ĐỐI không viết token ${cfg.consensusToken} trong bất kỳ lượt nào của bạn, kể cả khi bạn tin rằng mọi thứ đã xong hoặc chỉ muốn trích dẫn lại token.`,
      `Nhiệm vụ của bạn là tiếp thu góp ý và chỉnh sửa cho tới khi **${cfg.bLabel}** tự xác nhận.`,
    ].join(' ')
  }
  return [
    `Bạn là bên DUY NHẤT được phép chốt cuộc trao đổi, bằng cách kết thúc lượt bằng đúng token: ${cfg.consensusToken}.`,
    `Chỉ viết token khi nội dung **${cfg.aLabel}** vừa gửi đã thực sự ổn và bạn KHÔNG còn bất kỳ góp ý, yêu cầu chỉnh sửa, câu hỏi, điểm nghi ngờ hay việc cần làm thêm nào.`,
    `Nếu trong lượt này bạn còn nêu bất kỳ góp ý/vấn đề nào — dù nhỏ hay chỉ là "nên cân nhắc" — thì TUYỆT ĐỐI không viết token: hãy gửi góp ý về cho **${cfg.aLabel}** chỉnh sửa rồi xem xét lại ở lượt sau.`,
    'Không bao giờ vừa nêu góp ý vừa viết token trong cùng một lượt.',
  ].join(' ')
}

/** A's very first message. */
export function buildOpening(cfg: DuoPromptConfig): string {
  return joinSections([
    `Bạn là **${cfg.aLabel}**, sẽ trao đổi luân phiên từng lượt với **${cfg.bLabel}**.`,
    cfg.aInstruction,
    `Mục tiêu / nội dung cuộc trao đổi:\n${cfg.goal}`,
    'Hãy đưa ra lượt đầu tiên của bạn.',
    consensusNote(cfg, 'a'),
  ])
}

/** B's first message (carries A's opening reply). */
export function buildBOpening(cfg: DuoPromptConfig, aReply: string): string {
  return joinSections([
    `Bạn là **${cfg.bLabel}**, sẽ trao đổi luân phiên từng lượt với **${cfg.aLabel}**.`,
    cfg.bInstruction,
    `Mục tiêu / nội dung cuộc trao đổi:\n${cfg.goal}`,
    `**${cfg.aLabel}** vừa nói:\n\n${aReply}`,
    '→ Đây là lượt của bạn, hãy phản hồi.',
    consensusNote(cfg, 'b'),
  ])
}

/** A follow-up turn: relay the other side's reply to the `to` side. */
export function buildRelay(cfg: DuoPromptConfig, to: DuoSide, reply: string): string {
  const toLabel = to === 'a' ? cfg.aLabel : cfg.bLabel
  const fromLabel = to === 'a' ? cfg.bLabel : cfg.aLabel
  return joinSections([
    `**${fromLabel}** vừa nói:\n\n${reply}`,
    `→ Đây là lượt của bạn (**${toLabel}**), hãy phản hồi tiếp.`,
    consensusNote(cfg, to),
  ])
}

/**
 * Detect a consensus signal. Requires the token to appear standalone (on its own
 * line or at the very end), which is how the prompt instructs B to emit it, so
 * merely quoting it mid-sentence doesn't trigger a false stop.
 *
 * Callers must only run this on B's replies — A is never allowed to end the loop.
 */
export function hasConsensus(reply: string, token: string): boolean {
  if (!token) return false
  const text = reply.trim()
  if (!text) return false
  if (text.endsWith(token)) return true
  return text.split(/\r?\n/).some((line) => line.trim() === token)
}
