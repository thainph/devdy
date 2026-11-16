import { useI18n } from 'vue-i18n'
import { shortDuration } from '@/lib/workDashboard'

/** Localised compact durations for the Work Dashboard ("3h", "2 ngày"…). */
export function useDurationLabel() {
  const { t } = useI18n()

  /** `45m` / `3h` / `2d` in the UI language. */
  function duration(ms: number): string {
    const d = shortDuration(ms)
    return t(`dashboard.duration.${d.unit}`, { n: d.value })
  }

  /** "just now" under a minute, else the duration. */
  function ago(ms: number): string {
    return ms < 60_000 ? t('dashboard.justNow') : t('dashboard.ago', { duration: duration(ms) })
  }

  return { duration, ago }
}
