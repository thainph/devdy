/**
 * The Controller's error wording: a refusal / failure code from the Host (a
 * `command_ack.reason` or a `focus_ai_result.error`) in the user's language.
 * Focus codes reuse the desktop's `focus.ai.errors.*`; the phone-only ones
 * (`cancelled`, `timeout`, `invalid`…) live in `controller.focus.errors.*`; the
 * transport / data ones in `controller.data.errors.*`.
 */
import { useI18n } from 'vue-i18n'
import { focusErrorCode } from '@/lib/remoteFocus'

const DATA_ERRORS = new Set([
  'not_connected', 'connection_lost', 'host_ack_timeout', 'not_found', 'malformed',
  'rate_limited', 'action_not_allowed', 'failed', 'load_timeout',
])

export function useControllerErrors() {
  const { t, te } = useI18n()

  /** Any refusal reason (data write, Focus action) as a sentence. */
  function describe(reason: string | null): string {
    const raw = reason === 'disconnected' ? 'connection_lost' : reason
    if (raw && DATA_ERRORS.has(raw)) return t(`controller.data.errors.${raw}`)
    const code = raw ? focusErrorCode(raw) : null
    if (code && code !== 'failed') {
      if (te(`focus.ai.errors.${code}`)) return t(`focus.ai.errors.${code}`)
      if (te(`controller.focus.errors.${code}`)) return t(`controller.focus.errors.${code}`)
    }
    return t('controller.data.errors.generic', { reason: reason ?? '?' })
  }

  /** An AI request's failure code, or null for a cancel (nothing to say). */
  function aiError(code: string | null | undefined): string | null {
    if (!code || code === 'cancelled') return null
    if (code === 'failed') return t('controller.focus.errors.failed')
    return describe(code)
  }

  return { describe, aiError }
}
