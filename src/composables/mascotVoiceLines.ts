// The DY Cyber Fox's spoken lines, per app locale. Every line is read aloud by
// the browser's text-to-speech engine (see useMascotSpeech) and the same text is
// shown in the speech bubble, so the bubble mirrors exactly what the fox says.
//
// There are no recorded audio clips — all voices are synthesized via TTS, which
// lets every locale carry as many lines as we like. The tone is a concise,
// matter-of-fact AI assistant (no slang or over-casual chatter). The active
// language follows the app locale; any locale not listed here falls back to `en`
// (see pickMascotVoice).
import type { MascotBubbleVariant } from '@/composables/useMascotBubble'

export interface MascotVoiceLine {
  /** Verbatim words read aloud via TTS — shown in the bubble. */
  text: string
}

/** Variants that can carry a voice line, plus the "default" safety-net group. */
export type MascotVoiceKey = MascotBubbleVariant | 'default'

export type MascotVoiceLines = Partial<Record<MascotVoiceKey, MascotVoiceLine[]>>

/**
 * Voice lines per app locale. Falls back to `en` for any locale not listed here
 * (see pickMascotVoice).
 */
export const MASCOT_VOICE_LINES: Record<string, MascotVoiceLines> = {
  en: {
    thinking: [
      { text: 'Processing your request.' },
      { text: 'Analyzing the data. Please wait.' },
      { text: 'Computation in progress.' },
      { text: 'Querying and processing the information.' },
      { text: 'Executing the task now.' },
      { text: 'Compiling the results.' },
      { text: 'Task running. Nearing completion.' },
      { text: 'Evaluating the optimal approach.' },
      { text: 'Processor running at full capacity.' },
      { text: 'Initiating the processing routine.' },
      { text: 'Gathering and cross-checking the data.' },
      { text: 'Analyzing the logic. Stand by.' },
    ],
    permission: [
      { text: 'I require your authorization to proceed.' },
      { text: 'Confirmation required. May I execute this operation?' },
      { text: 'Standing by for your approval.' },
      { text: 'This operation requires your access permission.' },
      { text: 'Please confirm to continue execution.' },
      { text: 'Awaiting your approval command.' },
      { text: 'Your permission is needed to complete this task.' },
      { text: 'Awaiting confirmation before execution.' },
    ],
    success: [
      { text: 'Task complete.' },
      { text: 'Operation successful. You may review the results.' },
      { text: 'All operations completed.' },
      { text: 'Process finished with no errors.' },
      { text: 'The results are ready.' },
      { text: 'Execution successful.' },
      { text: 'Complete. Please review the output.' },
      { text: 'The task has been processed.' },
    ],
    error: [
      { text: 'An error was detected during processing.' },
      { text: 'Operation failed.' },
      { text: 'A fault occurred. Rechecking now.' },
      { text: 'Process interrupted by an error.' },
      { text: 'Unable to complete the task. Please check.' },
      { text: 'System reported an error. Your review is needed.' },
      { text: 'Execution failed. Analyzing the cause.' },
    ],
    info: [
      { text: 'Notice. An update is available for you.' },
      { text: 'Please take note of the following.' },
      { text: 'New data requires your attention.' },
      { text: 'System status update.' },
      { text: 'Information for you.' },
    ],
    // Safety net: used only when a variant above has no line of its own.
    default: [
      { text: 'The system has an update for you.' },
      { text: 'A new notification is available.' },
      { text: 'I have information to relay to you.' },
    ],
  },
  vi: {
    thinking: [
      { text: 'Đang xử lý yêu cầu của bạn.' },
      { text: 'Đang phân tích dữ liệu, vui lòng chờ trong giây lát.' },
      { text: 'Đang tính toán, tiến trình đang diễn ra.' },
      { text: 'Đang truy vấn và xử lý thông tin.' },
      { text: 'Hệ thống đang thực thi tác vụ.' },
      { text: 'Đang biên dịch kết quả.' },
      { text: 'Tiến trình đang chạy, sắp hoàn tất.' },
      { text: 'Đang đánh giá phương án tối ưu.' },
      { text: 'Bộ xử lý đang hoạt động ở công suất tối đa.' },
      { text: 'Đang khởi chạy quy trình xử lý.' },
      { text: 'Đang thu thập và đối chiếu dữ liệu.' },
      { text: 'Đang phân tích logic, vui lòng chờ.' },
    ],
    permission: [
      { text: 'Tôi cần bạn cấp quyền trước khi tiếp tục.' },
      { text: 'Yêu cầu xác nhận. Tôi có thể thực thi thao tác này không?' },
      { text: 'Tạm dừng, đang chờ phê duyệt từ bạn.' },
      { text: 'Thao tác này cần quyền truy cập của bạn.' },
      { text: 'Vui lòng xác nhận để tôi tiếp tục thực thi.' },
      { text: 'Hệ thống đang chờ lệnh phê duyệt.' },
      { text: 'Cần sự cho phép của bạn để hoàn tất tác vụ.' },
      { text: 'Đang chờ xác nhận trước khi thực hiện.' },
    ],
    success: [
      { text: 'Tác vụ đã hoàn tất.' },
      { text: 'Xử lý thành công, bạn có thể kiểm tra kết quả.' },
      { text: 'Đã hoàn thành toàn bộ thao tác.' },
      { text: 'Tiến trình kết thúc, không phát hiện lỗi.' },
      { text: 'Kết quả đã sẵn sàng.' },
      { text: 'Thực thi thành công.' },
      { text: 'Hoàn tất, vui lòng rà soát kết quả.' },
      { text: 'Tác vụ đã được xử lý xong.' },
    ],
    error: [
      { text: 'Đã phát hiện lỗi trong quá trình xử lý.' },
      { text: 'Thao tác không thành công.' },
      { text: 'Xảy ra sự cố, tôi đang kiểm tra lại.' },
      { text: 'Tiến trình bị gián đoạn do lỗi.' },
      { text: 'Không thể hoàn tất tác vụ, vui lòng kiểm tra.' },
      { text: 'Hệ thống báo lỗi, cần bạn xem xét.' },
      { text: 'Thực thi thất bại, đang phân tích nguyên nhân.' },
    ],
    info: [
      { text: 'Thông báo, có cập nhật dành cho bạn.' },
      { text: 'Vui lòng lưu ý thông tin sau.' },
      { text: 'Có dữ liệu mới cần bạn nắm.' },
      { text: 'Cập nhật trạng thái hệ thống.' },
      { text: 'Thông tin dành cho bạn.' },
    ],
    default: [
      { text: 'Hệ thống có cập nhật dành cho bạn.' },
      { text: 'Có thông báo mới.' },
      { text: 'Tôi có thông tin cần gửi đến bạn.' },
    ],
  },
}

function randomOf<T>(list: T[]): T {
  return list.length === 1 ? list[0] : list[Math.floor(Math.random() * list.length)]
}

/**
 * Pick a voice line for a variant in the given locale. Falls back to the locale's
 * "default" group, then to English, so the bubble always has words. Returns null
 * only when nothing at all is defined.
 */
export function pickMascotVoice(
  variant: MascotBubbleVariant,
  locale = 'en',
): MascotVoiceLine | null {
  const pickFrom = (lines: MascotVoiceLines | undefined) => {
    if (!lines) return null
    const own = lines[variant] ?? []
    if (own.length) return randomOf(own)
    const fallback = lines.default ?? []
    return fallback.length ? randomOf(fallback) : null
  }
  return pickFrom(MASCOT_VOICE_LINES[locale]) ?? pickFrom(MASCOT_VOICE_LINES.en)
}
