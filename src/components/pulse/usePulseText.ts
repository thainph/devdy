// Wording of the Pulse screen: health, deadline distance ("còn 3 ngày"), ball
// holder + age ("Bạn cần trả lời · 30h"), error chips and times.
import { useI18n } from 'vue-i18n'
import { ballAge, dueRelative, shortDate, KNOWN_ERRORS, type PulseDigest, type PulseNextDue } from '@/lib/pulse'

export function usePulseText() {
  const { t, te } = useI18n()

  function dueText(due: PulseNextDue | { dueAt: string } | null): string {
    if (!due) return ''
    const r = dueRelative(due.dueAt)
    const date = shortDate(due.dueAt)
    if (!r) return date
    if (r.kind === 'today') return `${date} (${t('pulse.due.today')})`
    if (r.kind === 'left') return `${date} (${t('pulse.due.left', { n: r.days })})`
    return `${date} (${r.days === 0 ? t('pulse.due.lateToday') : t('pulse.due.late', { n: r.days })})`
  }

  /** "Bạn cần trả lời · 30h" / "Chờ khách · 6 ngày"; '' when nobody is waited on. */
  function ballText(d: PulseDigest | null): string {
    if (!d || d.status === 'done' || (d.ball !== 'me' && d.ball !== 'customer' && d.ball !== 'team')) return ''
    const age = ballAge(d.ballSince)
    if (!age) return t(`pulse.ball.${d.ball}NoAge`)
    return t(`pulse.ball.${d.ball}`, { age: t(`pulse.age.${age.unit}`, { n: age.value }) })
  }

  function errorText(code: string | null): string {
    if (!code) return ''
    if ((KNOWN_ERRORS as readonly string[]).includes(code)) return t(`pulse.errors.${code}`)
    if (te(`slackThread.errors.${code}`)) return t(`slackThread.errors.${code}`, { detail: code })
    return t('pulse.errors.unknown', { detail: code })
  }

  /** `HH:mm` local. */
  function clock(ms: number | string): string {
    const d = new Date(ms)
    if (Number.isNaN(d.getTime())) return ''
    return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`
  }

  /** `dd/MM HH:mm` local. */
  function stamp(iso: string | null): string {
    return iso ? `${shortDate(iso)} ${clock(iso)}` : ''
  }

  return { dueText, ballText, errorText, clock, stamp }
}
