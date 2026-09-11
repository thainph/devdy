/**
 * Prompt templates for the Duo Session feature (two AI sessions that take turns
 * talking to each other in a loop). Kept dependency-free so both the
 * orchestrator store and any tests can import it without pulling in Tauri/Pinia.
 *
 * The templates are GENERIC: the two sides are just "A" (goes first) and "B"
 * (responds), each with a caller-supplied role instruction and label. Nothing
 * here assumes a design/review use-case — that (or rock-paper-scissors, a
 * debate, etc.) comes entirely from the goal + role instructions the caller
 * passes in. An optional consensus token lets either side end the loop.
 */

export interface DuoPromptConfig {
  /** The shared task/topic that seeds the whole conversation. */
  goal: string
  /** Display names for the two sides (injected into prompts). */
  aLabel: string
  bLabel: string
  /** Role behaviour for each side (free text; may be empty). */
  aInstruction: string
  bInstruction: string
  /** Token a side emits when the objective is met (empty = no consensus stop). */
  consensusToken: string
}

/** Join non-empty sections with blank lines between them. */
function joinSections(parts: string[]): string {
  return parts.map((p) => p.trim()).filter(Boolean).join('\n\n')
}

/** The consensus instruction, or '' when no token is configured. */
function consensusNote(cfg: DuoPromptConfig): string {
  if (!cfg.consensusToken) return ''
  return `Khi mục tiêu đã hoàn tất và bạn không còn gì cần bổ sung, hãy kết thúc lượt bằng đúng token: ${cfg.consensusToken}. Chỉ viết token khi thực sự hoàn tất; nếu chưa, TUYỆT ĐỐI không viết token.`
}

/** A's very first message. */
export function buildOpening(cfg: DuoPromptConfig): string {
  return joinSections([
    `Bạn là **${cfg.aLabel}**, sẽ trao đổi luân phiên từng lượt với **${cfg.bLabel}**.`,
    cfg.aInstruction,
    `Mục tiêu / nội dung cuộc trao đổi:\n${cfg.goal}`,
    'Hãy đưa ra lượt đầu tiên của bạn.',
    consensusNote(cfg),
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
    consensusNote(cfg),
  ])
}

/** A follow-up turn: relay `fromLabel`'s reply to the `toLabel` side. */
export function buildRelay(
  cfg: DuoPromptConfig,
  toLabel: string,
  fromLabel: string,
  reply: string,
): string {
  return joinSections([
    `**${fromLabel}** vừa nói:\n\n${reply}`,
    `→ Đây là lượt của bạn (**${toLabel}**), hãy phản hồi tiếp.`,
    consensusNote(cfg),
  ])
}

/**
 * Detect a consensus signal. Requires the token to appear standalone (on its own
 * line or at the very end), which is how the prompt instructs a side to emit it,
 * so merely quoting it mid-sentence doesn't trigger a false stop.
 */
export function hasConsensus(reply: string, token: string): boolean {
  if (!token) return false
  const text = reply.trim()
  if (!text) return false
  if (text.endsWith(token)) return true
  return text.split(/\r?\n/).some((line) => line.trim() === token)
}
