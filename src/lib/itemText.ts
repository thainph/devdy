import { i18n } from '@/i18n'

/** A todo / note's one-line label: the first non-empty line, stripped of markdown noise. */
export function firstLine(text: string): string {
  const line = text.split('\n').find((l) => l.trim()) ?? ''
  return line
    .replace(/^\s*[-*+]\s+\[[ xX]\]\s*/, '') // checklist marker
    .replace(/^\s*[-*+]\s+/, '') // bullet
    .replace(/^\s*#{1,6}\s+/, '') // heading
    .replace(/^\s*>\s?/, '') // quote
    .trim()
}

/** Compact "2h", "3d" style age of an item, falling back to a short date. */
export function relativeAge(iso: string): string {
  const t = i18n.global.t
  const diff = Date.now() - new Date(iso).getTime()
  const mins = Math.round(diff / 60000)
  if (mins < 1) return t('item.justNow')
  if (mins < 60) return t('item.minsAgo', { mins })
  const hours = Math.round(mins / 60)
  if (hours < 24) return t('item.hoursAgo', { hours })
  const days = Math.round(hours / 24)
  if (days < 30) return t('item.daysAgo', { days })
  return new Date(iso).toLocaleDateString(undefined, { month: 'short', day: 'numeric' })
}
