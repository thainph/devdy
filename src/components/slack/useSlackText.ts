// Wording shared by the Slack threads window and the composer picker: error
// codes (spec §4 / §8.3 + command codes), warnings (§8.4), job status / phase.
// Linked-session labels are composables/useSessionText.
import { useI18n } from 'vue-i18n'
import type {
  SlackFetchStatus,
  SlackThreadProgress,
  SlackThreadSummary,
} from '@/stores/slackThreads'

export function useSlackText() {
  const { t, te } = useI18n()

  /** Human text for an error code; unknown codes fall back to the raw detail. */
  function errorText(code: string | null, detail?: string | null): string {
    const d = detail ?? ''
    if (code && te(`slackThread.errors.${code}`)) {
      return t(`slackThread.errors.${code}`, { detail: d || code })
    }
    return t('slackThread.errors.unknown', { detail: d || code || '?' })
  }

  function warningText(code: string): string {
    return te(`slackThread.warnings.${code}`) ? t(`slackThread.warnings.${code}`) : code
  }

  function statusLabel(status: SlackFetchStatus): string {
    return t(`slackThread.status.${status}`)
  }

  /** "Downloading files 3/8"; without a progress event, the status alone. */
  function progressLabel(
    thread: Pick<SlackThreadSummary, 'fetchStatus'>,
    progress?: SlackThreadProgress | null,
  ): string {
    if (!progress || progress.status !== thread.fetchStatus) return statusLabel(thread.fetchStatus)
    const base = t(`slackThread.phase.${progress.phase}`)
    return progress.total != null && progress.phase !== 'done'
      ? `${base} ${progress.done}/${progress.total}`
      : `${base}…`
  }

  return { errorText, warningText, statusLabel, progressLabel }
}
