// Labels of a session (run) linked to a Slack thread / note / todo — shared by
// LinkedSessionsMenu and every window that lists linked sessions.
import { useI18n } from 'vue-i18n'
import type { SessionLink } from '@/lib/sessionLinks'

export function useSessionText() {
  const { t } = useI18n()

  /** A session's name: its title, else "Untitled session" / "Issue #N" / "PR #N". */
  function runLabel(run: Pick<SessionLink, 'title' | 'runType' | 'refNumber'>): string {
    if (run.runType === 'session' || run.refNumber == null) {
      return run.title?.trim() || t('sessionLink.untitledSession')
    }
    const kind = run.runType === 'analyze_issue' ? t('run.labelIssue') : t('run.labelPr')
    return `${kind} #${run.refNumber}`
  }

  /** Run status (fetched / running / done / failed / cancelled); a fetched session is a draft. */
  function runStatusLabel(run: Pick<SessionLink, 'status' | 'runType'>): string {
    if (run.status === 'fetched' && run.runType === 'session') return t('common.status.draft')
    return t(`common.status.${run.status}`)
  }

  return { runLabel, runStatusLabel }
}
