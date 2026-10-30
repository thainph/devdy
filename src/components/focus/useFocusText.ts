// Wording of the Focus board in the user's language: relative due labels, spans,
// estimates and the "why here" tooltip built from the backend's reasons (BR-05).
import { useI18n } from 'vue-i18n'
import {
  dueLabel,
  formatEstimate,
  spanOf,
  type DueLabel,
  type FocusItem,
  type FocusReason,
} from '@/lib/focus'

export function useFocusText() {
  const { t, te, locale } = useI18n()

  function span(minutes: number): string {
    const s = spanOf(minutes)
    return t(`focus.span.${s.unit}`, { n: s.value }, s.value)
  }

  /** `dd/MM` (weekday when within the coming week). */
  function shortDate(ymd: string, now: number): string {
    const [y, m, d] = ymd.split('-').map(Number)
    const date = new Date(y, m - 1, d)
    const days = (date.getTime() - new Date(new Date(now).toDateString()).getTime()) / 86_400_000
    if (days > 0 && days < 7) return date.toLocaleDateString(locale.value, { weekday: 'short' })
    return `${String(d).padStart(2, '0')}/${String(m).padStart(2, '0')}`
  }

  function dueText(label: DueLabel, now: number): string {
    switch (label.kind) {
      case 'overdue':
        return t('focus.due.overdue', { span: span(label.minutes) })
      case 'start_by':
        return t(label.late ? 'focus.due.startByLate' : 'focus.due.startBy', { time: label.time })
      case 'due':
        if (label.day === 'today') return label.time ? t('focus.due.todayAt', { time: label.time }) : t('focus.due.today')
        if (label.day === 'tomorrow') return label.time ? t('focus.due.tomorrowAt', { time: label.time }) : t('focus.due.tomorrow')
        return label.time
          ? t('focus.due.dateAt', { date: shortDate(label.date, now), time: label.time })
          : shortDate(label.date, now)
    }
  }

  /** Row due label + its tone, or null without a deadline. */
  function due(item: FocusItem, now: number): { text: string; tone: 'danger' | 'warn' | 'muted' } | null {
    const label = dueLabel(item, now)
    if (!label) return null
    const tone =
      label.kind === 'overdue' || (label.kind === 'start_by' && label.late)
        ? 'danger'
        : label.kind === 'start_by' || (label.kind === 'due' && label.day === 'today')
          ? 'warn'
          : 'muted'
    return { text: dueText(label, now), tone }
  }

  function reasonText(r: FocusReason): string {
    const n = typeof r.value === 'number' ? r.value : Number(r.value)
    switch (r.kind) {
      case 'due_in':
      case 'overdue_by':
        return Number.isFinite(n) ? t(`focus.reasons.${r.kind}`, { span: span(n) }) : t(`focus.reasons.${r.kind}`, { span: '?' })
      case 'needs':
        return t('focus.reasons.needs', { estimate: formatEstimate(n) })
      case 'slack':
        return Number.isFinite(n) && n < 0
          ? t('focus.reasons.slackNegative', { span: span(n) })
          : t('focus.reasons.slack', { span: span(n || 0) })
      case 'priority':
        return t('focus.reasons.priority', { n: r.value ?? '' })
      case 'aging':
        return t('focus.reasons.aging', { n }, n)
      default:
        if (te(`focus.reasons.${r.kind}`) && r.value === undefined) return t(`focus.reasons.${r.kind}`)
        return r.value === undefined ? r.kind : t('focus.reasons.unknown', { kind: r.kind, value: String(r.value) })
    }
  }

  /** "Why here" tooltip: the reasons joined with " · ". */
  function whyText(item: FocusItem): string {
    return item.reasons.map(reasonText).join(' · ')
  }

  /** Local date-time of an RFC3339 instant. */
  function stamp(iso: string | null | undefined): string {
    const d = iso ? new Date(iso) : null
    if (!d || Number.isNaN(d.getTime())) return ''
    return d.toLocaleString(locale.value, { day: '2-digit', month: '2-digit', hour: '2-digit', minute: '2-digit' })
  }

  return { span, due, dueText, reasonText, whyText, stamp, shortDate }
}
