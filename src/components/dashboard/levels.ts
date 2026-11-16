import type { Component } from 'vue'
import {
  Activity, CircleAlert, CircleCheck, CircleDashed, Flame, FolderKanban, GitPullRequest, ListTodo, MessagesSquare,
  TriangleAlert,
} from 'lucide-vue-next'
import type { CategoryKey, Level } from '@/lib/dashboardLayout'
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

/** Colour of a status icon: a severity, or a neutral accent. */
export type Tone = Level | 'primary' | 'info'

/** Tinted icon box (`StatusIcon`): a light fill and the icon in the tone. */
export const TONE_TILE: Record<Tone, string> = {
  bad: 'bg-destructive/15 text-destructive',
  warn: 'bg-warning/15 text-warning',
  ok: 'bg-success/15 text-success',
  none: 'bg-muted text-muted-foreground',
  primary: 'bg-primary/15 text-primary',
  info: 'bg-info/15 text-info',
}

/** Severity in one icon, always next to its words. */
export const LEVEL_ICON: Record<Level, Component> = {
  bad: CircleAlert,
  warn: TriangleAlert,
  ok: CircleCheck,
  none: CircleDashed,
}

/** A project row's icon: at risk burns, attention warns, on track checks, unknown is dashed. */
export const HEALTH_ICON: Record<HealthLevel, Component> = {
  red: Flame,
  orange: TriangleAlert,
  green: CircleCheck,
  gray: CircleDashed,
}

export const CATEGORY_ICON: Record<CategoryKey | 'runs', Component> = {
  slack: MessagesSquare,
  todos: ListTodo,
  prs: GitPullRequest,
  projects: FolderKanban,
  runs: Activity,
}
