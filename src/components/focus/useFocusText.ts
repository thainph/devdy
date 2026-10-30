// Wording of the Focus board in the user's language: relative due labels, spans,
// estimates and the "why here" tooltip built from the backend's reasons (BR-05).
import { useI18n } from 'vue-i18n'
import {
  dayDiff,
  dueLabel,
  parseReason,
  formatEstimate,
  spanOf,
  type DueLabel,
  type FocusItem,
  type FocusReason,
} from '@/lib/focus'

export function useFocusText() {
  const { t, locale } = useI18n()

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

  /** "15:00 today" / "tomorrow 15:00" / "Mon 15:00" of a local `date` + `time`. */
  function whenText(date: string, time: string | null, now: number): string {
    const diff = dayDiff(new Date(`${date}T00:00`).getTime(), now)
    if (diff === 0) return time ? t('focus.due.todayAt', { time }) : t('focus.due.today')
    if (diff === 1) return time ? t('focus.due.tomorrowAt', { time }) : t('focus.due.tomorrow')
    const day = shortDate(date, now)
    return time ? t('focus.due.dateAt', { date: day, time }) : day
  }

  function reasonText(r: FocusReason, now: number): string {
    const v = parseReason(r)
    switch (v.kind) {
      case 'doing':
      case 'waiting':
      case 'no_due':
      case 'done':
        return t(`focus.reasons.${v.kind}`)
      case 'snoozed':
      case 'due':
        if (!v.date) return v.kind === 'snoozed' ? t('focus.reasons.snoozedNoTime') : t('focus.reasons.no_due')
        return t(`focus.reasons.${v.kind}`, { when: whenText(v.date, v.time, now) })
      case 'overdue':
      case 'work_left':
      case 'late_by':
        return t(`focus.reasons.${v.kind}`, { span: span(v.minutes) })
      case 'estimate':
      case 'estimate_missing':
        return t(`focus.reasons.${v.kind}`, { estimate: formatEstimate(v.minutes) })
      case 'priority':
      case 'age_days':
        return t(`focus.reasons.${v.kind}`, { n: v.n })
      case 'unknown':
        return v.value == null ? v.name : t('focus.reasons.unknown', { kind: v.name, value: v.value })
    }
  }

  /** "Why here" tooltip: the reasons joined with " · ". */
  function whyText(item: FocusItem, now: number): string {
    return item.reasons.map((r) => reasonText(r, now)).join(' · ')
  }

  /** Local date-time of an RFC3339 instant. */
  function stamp(iso: string | null | undefined): string {
    const d = iso ? new Date(iso) : null
    if (!d || Number.isNaN(d.getTime())) return ''
    return d.toLocaleString(locale.value, { day: '2-digit', month: '2-digit', hour: '2-digit', minute: '2-digit' })
  }

  return { span, due, dueText, reasonText, whyText, stamp, shortDate }
}
