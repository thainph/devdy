import type { Level } from '@/lib/dashboardLayout'
import type { HealthLevel } from '@/lib/projectHealth'

// Theme tokens per severity, as full class strings so Tailwind sees them.
// Every theme (and light / dark) redefines --destructive / --warning /
// --success, and `.ui-badge-*` is restyled per theme, so nothing here is a
// fixed colour.

/** Big-number / emphasis text. */
export const LEVEL_TEXT: Record<Level, string> = {
  bad: 'text-destructive',
  warn: 'text-warning',
  ok: 'text-foreground',
  none: 'text-muted-foreground',
}

/** Severity stripe (summary tiles, project rows). */
export const LEVEL_STRIPE: Record<Level, string> = {
  bad: 'bg-destructive',
  warn: 'bg-warning',
  ok: 'bg-success',
  none: 'bg-border',
}

export type BadgeTone = 'error' | 'warning' | 'success' | 'neutral'

export const LEVEL_BADGE: Record<Level, BadgeTone> = {
  bad: 'error',
  warn: 'warning',
  ok: 'success',
  none: 'neutral',
}

export const HEALTH_LEVEL: Record<HealthLevel, Level> = {
  red: 'bad',
  orange: 'warn',
  green: 'ok',
  gray: 'none',
}
