// Bridge between the app's local `YYYY-MM-DD` strings and the
// `@internationalized/date` values radix-vue's Calendar components expect.
import { CalendarDate, DateFormatter, getLocalTimeZone, parseDate, today } from '@internationalized/date'

/** Parse a `YYYY-MM-DD` string; returns undefined for empty / malformed input. */
export function ymdToDate(ymd: string | null | undefined): CalendarDate | undefined {
  if (!ymd) return undefined
  try {
    return parseDate(ymd)
  } catch {
    return undefined
  }
}

/** Serialize back to `YYYY-MM-DD` (CalendarDate#toString is ISO date-only). */
export function dateToYmd(d: { toString(): string } | null | undefined): string {
  return d ? d.toString().slice(0, 10) : ''
}

export function todayDate(): CalendarDate {
  return today(getLocalTimeZone())
}

/** App locale (`en` | `vi`) → BCP 47 tag used by Intl + radix-vue. */
export function intlLocale(locale: string): string {
  return locale === 'vi' ? 'vi-VN' : 'en-US'
}

export function formatDate(
  d: CalendarDate,
  locale: string,
  options: Intl.DateTimeFormatOptions,
): string {
  return new DateFormatter(intlLocale(locale), options).format(d.toDate(getLocalTimeZone()))
}

/** Locale-aware range label, e.g. "Oct 10 – 16, 2026". */
export function formatDateRange(
  start: CalendarDate,
  end: CalendarDate,
  locale: string,
  options: Intl.DateTimeFormatOptions,
): string {
  const tz = getLocalTimeZone()
  return new DateFormatter(intlLocale(locale), options).formatRange(start.toDate(tz), end.toDate(tz))
}
