import { useI18n } from 'vue-i18n'
import type { HealthReason } from '@/lib/dashboardLayout'
import type { ProjectHealth } from '@/lib/projectHealth'

/** A project's level reasons (`healthReasons`) and gray reason, in words. */
export function useHealthWords() {
  const { t } = useI18n()

  function reasonText(r: HealthReason): string {
    switch (r.kind) {
      case 'issue': {
        const issue = `#${r.number} ${r.title}`
        if (r.radar === 'overdue') {
          const n = -(r.daysLeft ?? 0)
          return t('dashboard.health.why.issueOverdue', { issue, n }, n)
        }
        if (r.radar === 'due-soon') {
          const n = r.daysLeft ?? 0
          return t('dashboard.health.why.issueDueSoon', { issue, n }, n)
        }
        return t(`dashboard.health.why.issue.${r.radar}`, { issue })
      }
      case 'milestoneOverdue':
        return t('dashboard.health.why.milestoneOverdue', { title: r.title, n: r.lateDays }, r.lateDays)
      case 'milestoneLag':
        return t('dashboard.health.why.milestoneLag', { title: r.title, n: r.lagPct })
      case 'riskyIssues':
        return t('dashboard.health.why.riskyIssues', { n: r.count }, r.count)
    }
  }

  function grayReason(h: ProjectHealth): string {
    return t(`dashboard.health.reason.${h.reason ?? 'loading'}`)
  }

  return { reasonText, grayReason }
}
