/**
 * Display helpers shared by the session lists — RunView's History rows and the
 * worker panel — so a run reads the same wherever it is listed.
 */
import { Circle, CircleCheck, CircleDot, CircleSlash, CircleX, Loader2 } from 'lucide-vue-next'

/** Drop a leading "claude-" from a model id for compact rows (the account badge
 *  already implies Claude); callers keep the full id in a tooltip. */
export function shortModelId(model: string): string {
  return model.replace(/^claude-/, '')
}

/** Compact span: "5m", "2h", "3d", "2w", "4mo", "1y". Under a minute returns
 *  `underMinute` (e.g. a localized "now"). */
export function compactSpan(ms: number, underMinute: string): string {
  if (Number.isNaN(ms)) return ''
  const min = Math.floor(Math.max(0, ms) / 60_000)
  if (min < 1) return underMinute
  if (min < 60) return `${min}m`
  const h = Math.floor(min / 60)
  if (h < 24) return `${h}h`
  const d = Math.floor(h / 24)
  if (d < 7) return `${d}d`
  if (d < 30) return `${Math.floor(d / 7)}w`
  if (d < 365) return `${Math.floor(d / 30)}mo`
  return `${Math.floor(d / 365)}y`
}

/** Relative age of `iso` (time since), via compactSpan. */
export function relativeAge(iso: string, underMinute: string, now = Date.now()): string {
  return compactSpan(now - new Date(iso).getTime(), underMinute)
}

/** Full date + time, so runs from the same day stay distinguishable. Falls back
 *  to the raw string if the date can't be parsed. */
export function formatRunTimestamp(iso: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return iso
  return d.toLocaleString(undefined, {
    year: 'numeric',
    month: 'numeric',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  })
}

/** Status glyph for a run row (a spinner while running). */
export function runStatusIcon(status: string) {
  switch (status) {
    case 'running': return Loader2
    case 'done': return CircleCheck
    case 'failed': return CircleX
    case 'cancelled': return CircleSlash
    case 'fetched': return CircleDot
    default: return Circle
  }
}

/** Status tint, mirroring StatusBadge's palette. */
export function runStatusColorClass(status: string): string {
  switch (status) {
    case 'running': return 'text-blue-500'
    case 'done': return 'text-emerald-500'
    case 'failed': return 'text-red-500'
    case 'cancelled': return 'text-amber-500'
    case 'fetched': return 'text-violet-500'
    default: return 'text-muted-foreground'
  }
}

/** Solid dot colour for the same palette (the collapsed worker rail). */
export function runStatusDotClass(status: string): string {
  switch (status) {
    case 'running': return 'bg-blue-500'
    case 'done': return 'bg-emerald-500'
    case 'failed': return 'bg-red-500'
    case 'cancelled': return 'bg-amber-500'
    case 'fetched': return 'bg-violet-500'
    default: return 'bg-muted-foreground'
  }
}
